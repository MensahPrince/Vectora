//! cutlass-download: the project's **one** downloader.
//!
//! Everything the app pulls over the network at runtime — AI model weights,
//! sound packs, effect bundles, project templates — flows through here so the
//! concerns every download shares live in exactly one place:
//!
//! - **Atomic publish.** Bytes land in a `*.part` sibling and are `rename`d
//!   onto the final path only after the whole transfer (and any checksum)
//!   succeeds, so a reader never sees a half-written file.
//! - **Resume.** An interrupted transfer leaves its `*.part` on disk; the next
//!   attempt sends a `Range` request and continues where it stopped. Servers
//!   that ignore `Range` (reply `200`) restart cleanly.
//! - **Integrity.** An optional SHA-256 is streamed *while writing* (no extra
//!   read pass on a fresh download) and verified before publish. A matching
//!   file already at the destination is a cache hit — verified, then skipped.
//! - **Resilience.** Transport errors, truncated bodies, and retryable HTTP
//!   statuses back off exponentially and retry; resume means a retry doesn't
//!   re-fetch what already arrived. `404`/`401`/`403`-class statuses are fatal.
//! - **Concurrency across files.** [`Downloader::fetch_many`] runs a bounded
//!   pool of OS threads, reusing one connection-pooling [`ureq::Agent`].
//! - **Concurrent connections per file.** A single large file on a range-capable
//!   server is split into fixed-size chunks pulled over several connections at
//!   once (aria2c-style), then written straight to their offsets. The chunk is
//!   also the resume unit: a finished chunk stays finished across restarts,
//!   tracked in a tiny `*.part.progress` manifest beside the data.
//!
//! ## Why blocking
//!
//! Like the AI provider seam (`cutlass-ai`), this is **synchronous by design**
//! — it keeps `tokio` out of the app. Callers run a [`Downloader`] on a worker
//! thread (never the UI thread) and observe progress through a callback.
//!
//! Small files (and servers that don't advertise byte ranges) transparently
//! fall back to a single stream — set [`DownloadOptions::max_connections`] to
//! `1` to force that everywhere and skip the size probe entirely (ideal for the
//! many-small-files case: sounds, effects, templates).
//!
//! ```no_run
//! use std::sync::atomic::AtomicBool;
//! use cutlass_download::{Downloader, DownloadRequest};
//!
//! let dl = Downloader::default();
//! let cancel = AtomicBool::new(false);
//! let req = DownloadRequest::new("https://example.com/model.gguf", "/models/model.gguf")
//!     .with_sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
//! dl.fetch(&req, &cancel, &mut |p| {
//!     if let Some(f) = p.fraction() {
//!         println!("{:.0}%", f * 100.0);
//!     }
//! })?;
//! # Ok::<(), cutlass_download::DownloadError>(())
//! ```

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tracing::warn;

/// Streaming copy / hash buffer. Large enough to keep syscall overhead low on
/// multi-GB weights, small enough that a full pool of concurrent fetches stays
/// modest in resident memory.
const READ_BUFFER: usize = 256 * 1024;

/// Upper bound on a single backoff sleep.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Minimum spacing between progress callbacks, so a fast link can't flood the
/// caller (and, transitively, the UI thread) with updates.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// How often the parallel path flushes its resume manifest to disk while a
/// transfer is in flight.
const MANIFEST_INTERVAL: Duration = Duration::from_millis(500);

/// First line of the resume manifest; a version tag so a future format change
/// is rejected (and the transfer restarts) rather than misread.
const MANIFEST_HEADER: &str = "cutlass-download-resume v1";

/// An expected content digest, checked before the file is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    /// Lowercase or uppercase hex SHA-256.
    Sha256(String),
}

impl Checksum {
    fn matches(&self, digest_hex: &str) -> bool {
        match self {
            Checksum::Sha256(expected) => expected.eq_ignore_ascii_case(digest_hex),
        }
    }

    fn expected_hex(&self) -> &str {
        match self {
            Checksum::Sha256(s) => s,
        }
    }
}

/// One file to fetch: where from, where to, and (optionally) what it should be.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub dest: PathBuf,
    pub checksum: Option<Checksum>,
}

impl DownloadRequest {
    pub fn new(url: impl Into<String>, dest: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            dest: dest.into(),
            checksum: None,
        }
    }

    /// Require the downloaded bytes to hash to `hex` (SHA-256). Enables the
    /// "already present and valid -> skip" cache hit, and makes resume safe
    /// across a remote file changing underneath us.
    pub fn with_sha256(mut self, hex: impl Into<String>) -> Self {
        self.checksum = Some(Checksum::Sha256(hex.into()));
        self
    }
}

/// A point-in-time transfer measurement handed to the progress callback.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    /// Bytes on disk so far (includes bytes carried over by resume).
    pub downloaded: u64,
    /// Total size when the server reported one, else `None`.
    pub total: Option<u64>,
}

impl Progress {
    /// Completion in `0.0..=1.0` when the total size is known.
    pub fn fraction(&self) -> Option<f32> {
        match self.total {
            Some(0) | None => None,
            Some(total) => Some((self.downloaded as f32 / total as f32).min(1.0)),
        }
    }
}

/// Knobs shared by every fetch a [`Downloader`] performs.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Retries *after* the first attempt before giving up.
    pub max_retries: u32,
    /// TCP connect timeout per attempt.
    pub connect_timeout: Duration,
    /// Resume from an existing `*.part` via `Range` when possible.
    pub resume: bool,
    /// `User-Agent` sent on every request.
    pub user_agent: String,
    /// Base backoff; attempt *n* waits `initial_backoff * 2^n`, capped.
    pub initial_backoff: Duration,
    /// Maximum simultaneous connections used to fetch a *single* large file via
    /// parallel HTTP range requests. `1` disables per-file parallelism (and the
    /// size probe), giving plain single-stream downloads.
    pub max_connections: usize,
    /// Only files at least this large (on range-capable servers) are fetched
    /// with multiple connections; smaller files stay single-stream.
    pub min_parallel_size: u64,
    /// Byte granularity of each parallel range request, and the unit of resume:
    /// a chunk that finished stays finished across restarts.
    pub chunk_size: u64,
    /// Whole-request timeout for the size/range probe (a `HEAD`, then a 1-byte
    /// ranged `GET`). Bounds a slow or misbehaving server during detection so a
    /// probe can never turn into an unbounded transfer.
    pub probe_timeout: Duration,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            max_retries: 5,
            connect_timeout: Duration::from_secs(15),
            resume: true,
            user_agent: concat!("cutlass-download/", env!("CARGO_PKG_VERSION")).to_string(),
            initial_backoff: Duration::from_millis(500),
            max_connections: 8,
            min_parallel_size: 8 * 1024 * 1024,
            chunk_size: 8 * 1024 * 1024,
            probe_timeout: Duration::from_secs(15),
        }
    }
}

/// Everything that can go wrong fetching a file.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("download cancelled")]
    Cancelled,

    #[error("network error for {url}: {message}")]
    Network { url: String, message: String },

    #[error("server returned HTTP {status} for {url}")]
    Http { url: String, status: u16 },

    #[error("checksum mismatch for {}: expected {expected}, got {actual}", dest.display())]
    Checksum {
        dest: PathBuf,
        expected: String,
        actual: String,
    },

    #[error("io error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("gave up on {url} after {attempts} attempt(s): {last}")]
    Exhausted {
        url: String,
        attempts: u32,
        last: String,
    },
}

impl DownloadError {
    /// Errors we never retry: cancellation and client-side HTTP statuses where
    /// trying again can only fail the same way.
    fn is_fatal(&self) -> bool {
        match self {
            DownloadError::Cancelled => true,
            DownloadError::Http { status, .. } => is_fatal_status(*status),
            _ => false,
        }
    }
}

/// A reusable, connection-pooling downloader. Construct once, fetch many.
pub struct Downloader {
    agent: ureq::Agent,
    opts: DownloadOptions,
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new(DownloadOptions::default())
    }
}

impl Downloader {
    pub fn new(opts: DownloadOptions) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(opts.connect_timeout)
            .user_agent(&opts.user_agent)
            .build();
        Self { agent, opts }
    }

    pub fn options(&self) -> &DownloadOptions {
        &self.opts
    }

    /// Fetch one file, blocking until it is published, cancelled, or fails.
    /// On success the returned path is `req.dest`.
    pub fn fetch(
        &self,
        req: &DownloadRequest,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<PathBuf, DownloadError> {
        fetch_with_retry(&self.agent, &self.opts, req, cancel, on_progress)
    }

    /// Fetch many files with at most `concurrency` in flight at once, reusing
    /// one agent. Results are returned positionally — `out[i]` is the outcome
    /// of `reqs[i]`.
    ///
    /// `on_progress` is called as `(index, progress)` and may fire from several
    /// worker threads at once, so it must be `Sync`; keep it cheap and
    /// non-blocking (e.g. post to a channel).
    pub fn fetch_many(
        &self,
        reqs: &[DownloadRequest],
        concurrency: usize,
        cancel: &AtomicBool,
        on_progress: &(dyn Fn(usize, Progress) + Sync),
    ) -> Vec<Result<PathBuf, DownloadError>> {
        let n = reqs.len();
        let mut results: Vec<Result<PathBuf, DownloadError>> = Vec::with_capacity(n);
        results.resize_with(n, || Err(DownloadError::Cancelled));
        if n == 0 {
            return results;
        }

        let workers = concurrency.clamp(1, n);
        let next = AtomicUsize::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<PathBuf, DownloadError>)>();

        thread::scope(|scope| {
            for _ in 0..workers {
                let tx = tx.clone();
                let next = &next;
                let agent = &self.agent;
                let opts = &self.opts;
                scope.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        let outcome = if cancel.load(Ordering::Relaxed) {
                            Err(DownloadError::Cancelled)
                        } else {
                            let mut progress = |p: Progress| on_progress(i, p);
                            fetch_with_retry(agent, opts, &reqs[i], cancel, &mut progress)
                        };
                        let _ = tx.send((i, outcome));
                    }
                });
            }
            // Drop the original sender so `rx` closes once every worker's clone
            // is gone (i.e. all threads have exited).
            drop(tx);
        });

        for (i, outcome) in rx.iter() {
            results[i] = outcome;
        }
        results
    }
}

/// One attempt with the retry/backoff loop wrapped around it.
fn fetch_with_retry(
    agent: &ureq::Agent,
    opts: &DownloadOptions,
    req: &DownloadRequest,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<PathBuf, DownloadError> {
    // Cache hit: a file already sitting at the destination that matches the
    // expected checksum needs no network at all.
    if let Some(checksum) = &req.checksum {
        if req.dest.is_file() {
            if let Ok(actual) = hash_file(&req.dest) {
                if checksum.matches(&actual) {
                    let len = fs::metadata(&req.dest).map(|m| m.len()).unwrap_or(0);
                    on_progress(Progress {
                        downloaded: len,
                        total: Some(len),
                    });
                    return Ok(req.dest.clone());
                }
            }
        }
    }

    if let Some(parent) = req.dest.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
    }
    let temp = temp_path(&req.dest);

    let mut attempt: u32 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }

        match attempt_fetch(agent, opts, req, &temp, cancel, on_progress) {
            Ok(digest) => {
                if let Some(checksum) = &req.checksum {
                    // Single-stream hashes while writing; the parallel path
                    // writes out of order, so hash the finished file instead.
                    let actual = match digest {
                        Some(streamed) => streamed,
                        None => hash_file(&temp).map_err(|e| io_err(&temp, e))?,
                    };
                    if !checksum.matches(&actual) {
                        // Corrupt transfer: drop the partial so the retry starts
                        // clean rather than resuming bad bytes.
                        let _ = fs::remove_file(&temp);
                        let err = DownloadError::Checksum {
                            dest: req.dest.clone(),
                            expected: checksum.expected_hex().to_string(),
                            actual,
                        };
                        if attempt >= opts.max_retries {
                            return Err(err);
                        }
                        warn!("{err}; retrying");
                        backoff(opts, attempt, cancel)?;
                        attempt += 1;
                        continue;
                    }
                }
                persist(&temp, &req.dest)?;
                return Ok(req.dest.clone());
            }
            Err(err) if err.is_fatal() => return Err(err),
            Err(err) => {
                if attempt >= opts.max_retries {
                    return Err(DownloadError::Exhausted {
                        url: req.url.clone(),
                        attempts: attempt + 1,
                        last: err.to_string(),
                    });
                }
                warn!(
                    "download attempt {} for {} failed: {err}; retrying",
                    attempt + 1,
                    req.url
                );
                backoff(opts, attempt, cancel)?;
                attempt += 1;
            }
        }
    }
}

/// Outcome of attempting the parallel (multi-connection) path.
enum ParallelOutcome {
    /// The whole file landed in the temp file (not yet published).
    Completed,
    /// Parallelism doesn't apply (small file, unknown size, or a server that
    /// won't honour byte ranges); the caller should fall back to a single
    /// stream.
    NotApplicable,
}

/// Choose the transfer strategy for one attempt: parallel range requests for a
/// large file on a capable server, otherwise a single stream. Returns the
/// streamed SHA-256 when the single-stream path computed one; the parallel
/// path returns `None` (its bytes land out of order, so the caller hashes the
/// finished file).
fn attempt_fetch(
    agent: &ureq::Agent,
    opts: &DownloadOptions,
    req: &DownloadRequest,
    temp: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Option<String>, DownloadError> {
    if opts.max_connections > 1 {
        match try_parallel(agent, opts, req, temp, cancel, on_progress)? {
            ParallelOutcome::Completed => return Ok(None),
            ParallelOutcome::NotApplicable => {}
        }
    }
    fetch_once(agent, opts, req, temp, cancel, on_progress)
}

/// What probing told us about a URL.
struct ServerInfo {
    total: Option<u64>,
    supports_range: bool,
}

/// Discover the file size and whether the server honours byte ranges.
///
/// `HEAD` is tried first (bodyless and cheap); if it both sizes the file and
/// advertises `Accept-Ranges: bytes`, we're done in one request. Otherwise —
/// `HEAD` may be unsupported (e.g. `405`), or simply silent about ranges even
/// though the server supports them — we fall back to a **single-byte ranged
/// `GET`** and read only the status line and headers:
///
///   * `206 Partial Content` -> ranges work; the full size is in `Content-Range`.
///   * anything else (typically `200`) -> the range was ignored and the server
///     is about to stream the *whole* file. We drop the response without
///     reading the body, so we never download it, and report no range support.
///
/// Every probe request is bounded by [`DownloadOptions::probe_timeout`], so a
/// probe can never become an unbounded transfer.
fn probe(agent: &ureq::Agent, opts: &DownloadOptions, url: &str) -> ServerInfo {
    if let Ok(response) = agent
        .request("HEAD", url)
        .timeout(opts.probe_timeout)
        .call()
    {
        let total = parse_len(response.header("Content-Length"));
        if accept_ranges_bytes(response.header("Accept-Ranges")) && total.is_some() {
            return ServerInfo {
                total,
                supports_range: true,
            };
        }
    }
    probe_with_range(agent, opts, url)
}

/// Settle range support with a one-byte `GET`, reading only the headers.
fn probe_with_range(agent: &ureq::Agent, opts: &DownloadOptions, url: &str) -> ServerInfo {
    match agent
        .get(url)
        .set("Range", "bytes=0-0")
        .timeout(opts.probe_timeout)
        .call()
    {
        Ok(response) if response.status() == 206 => ServerInfo {
            total: content_range_total(response.header("Content-Range")),
            supports_range: true,
        },
        // 200 (range ignored) or any other status: do NOT touch the body. Let
        // `response` drop here, which closes the connection before the server
        // can stream the whole file at us.
        Ok(response) => ServerInfo {
            total: parse_len(response.header("Content-Length")),
            supports_range: false,
        },
        Err(_) => ServerInfo {
            total: None,
            supports_range: false,
        },
    }
}

fn accept_ranges_bytes(header: Option<&str>) -> bool {
    header
        .map(|v| v.trim().eq_ignore_ascii_case("bytes"))
        .unwrap_or(false)
}

/// Fetch one file with up to `opts.max_connections` parallel range requests,
/// resuming any chunks recorded in the sidecar manifest. Bytes are written
/// straight to their final offsets in the temp file (no reassembly pass).
fn try_parallel(
    agent: &ureq::Agent,
    opts: &DownloadOptions,
    req: &DownloadRequest,
    temp: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<ParallelOutcome, DownloadError> {
    let info = probe(agent, opts, &req.url);
    let chunk_size = opts.chunk_size.max(1);
    // Need a known size, range support, a file worth splitting, and at least
    // two chunks for parallelism to mean anything.
    let total = match info.total {
        Some(t) if info.supports_range && t >= opts.min_parallel_size && t > chunk_size => t,
        _ => return Ok(ParallelOutcome::NotApplicable),
    };
    let num_chunks = total.div_ceil(chunk_size) as usize;
    if num_chunks < 2 {
        return Ok(ParallelOutcome::NotApplicable);
    }

    let manifest_path = append_suffix(temp, ".progress");

    // Open (or create) the temp and size it to the full length so every worker
    // can write its slice at the right offset.
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(temp)
        .map_err(|e| io_err(temp, e))?;
    file.set_len(total).map_err(|e| io_err(temp, e))?;
    let file = Arc::new(file);

    // Seed per-chunk progress from a matching resume manifest, if any.
    let mut seed = vec![0u64; num_chunks];
    if opts.resume {
        if let Some(manifest) = load_manifest(&manifest_path) {
            if manifest.total == total
                && manifest.chunk_size == chunk_size
                && manifest.done.len() == num_chunks
            {
                for (i, &done) in manifest.done.iter().enumerate() {
                    seed[i] = done.min(chunk_len(i, num_chunks, total, chunk_size));
                }
            }
        }
    } else {
        let _ = fs::remove_file(&manifest_path);
    }

    let done: Arc<Vec<AtomicU64>> = Arc::new(seed.iter().map(|&d| AtomicU64::new(d)).collect());
    let downloaded = AtomicU64::new(seed.iter().sum());
    let next = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let error: Mutex<Option<DownloadError>> = Mutex::new(None);
    let worker_count = opts.max_connections.clamp(1, num_chunks);
    let workers_alive = AtomicUsize::new(worker_count);
    let url = req.url.as_str();

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let file = Arc::clone(&file);
            let done = Arc::clone(&done);
            let next = &next;
            let abort = &abort;
            let error = &error;
            let downloaded = &downloaded;
            let workers_alive = &workers_alive;
            scope.spawn(move || {
                while !cancel.load(Ordering::Relaxed) && !abort.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= num_chunks {
                        break;
                    }
                    let len = chunk_len(i, num_chunks, total, chunk_size);
                    let already = done[i].load(Ordering::Relaxed);
                    if already >= len {
                        continue; // chunk finished on a previous run
                    }
                    let start = i as u64 * chunk_size;
                    let from = start + already;
                    let end = start + len - 1;
                    match download_range(
                        agent, url, from, end, &file, temp, &done[i], downloaded, cancel, abort,
                    ) {
                        Ok(()) => {}
                        Err(DownloadError::Cancelled) => break,
                        Err(e) => {
                            let mut slot = error.lock().unwrap();
                            if slot.is_none() {
                                *slot = Some(e);
                            }
                            abort.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                }
                workers_alive.fetch_sub(1, Ordering::Relaxed);
            });
        }

        // The scope's own thread drives progress and manifest persistence while
        // the workers run, so the user's callback is only ever touched here.
        let mut last_persist = Instant::now();
        loop {
            on_progress(Progress {
                downloaded: downloaded.load(Ordering::Relaxed),
                total: Some(total),
            });
            if workers_alive.load(Ordering::Relaxed) == 0 || cancel.load(Ordering::Relaxed) {
                break;
            }
            if last_persist.elapsed() >= MANIFEST_INTERVAL {
                persist_manifest(&manifest_path, total, chunk_size, done.as_slice());
                last_persist = Instant::now();
            }
            thread::sleep(PROGRESS_INTERVAL);
        }
    });

    if cancel.load(Ordering::Relaxed) {
        persist_manifest(&manifest_path, total, chunk_size, done.as_slice());
        return Err(DownloadError::Cancelled);
    }
    if let Some(err) = error.lock().unwrap().take() {
        persist_manifest(&manifest_path, total, chunk_size, done.as_slice());
        return Err(err);
    }

    let fetched: u64 = done.iter().map(|d| d.load(Ordering::Relaxed)).sum();
    if fetched < total {
        persist_manifest(&manifest_path, total, chunk_size, done.as_slice());
        return Err(DownloadError::Network {
            url: req.url.clone(),
            message: format!("parallel transfer incomplete: {fetched} of {total} bytes"),
        });
    }

    let _ = file.sync_all();
    on_progress(Progress {
        downloaded: total,
        total: Some(total),
    });
    let _ = fs::remove_file(&manifest_path);
    Ok(ParallelOutcome::Completed)
}

/// Byte length of chunk `i` (the last chunk is short unless the size divides
/// evenly).
fn chunk_len(i: usize, num_chunks: usize, total: u64, chunk_size: u64) -> u64 {
    if i + 1 == num_chunks {
        total - (i as u64 * chunk_size)
    } else {
        chunk_size
    }
}

/// Download the closed byte range `[from, end]` straight into `file` at offset
/// `from`, bumping the per-chunk and grand-total counters as bytes arrive.
#[allow(clippy::too_many_arguments)]
fn download_range(
    agent: &ureq::Agent,
    url: &str,
    from: u64,
    end: u64,
    file: &File,
    temp: &Path,
    done_chunk: &AtomicU64,
    downloaded: &AtomicU64,
    cancel: &AtomicBool,
    abort: &AtomicBool,
) -> Result<(), DownloadError> {
    let response = match agent
        .get(url)
        .set("Range", &format!("bytes={from}-{end}"))
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(status, _)) => {
            return Err(DownloadError::Http {
                url: url.to_string(),
                status,
            });
        }
        Err(ureq::Error::Transport(t)) => {
            return Err(DownloadError::Network {
                url: url.to_string(),
                message: t.to_string(),
            });
        }
    };
    // A capable server answers 206; a 200 here means the range was ignored and
    // our offset math would be wrong, so treat it as retryable.
    if response.status() != 206 {
        return Err(DownloadError::Network {
            url: url.to_string(),
            message: format!(
                "expected HTTP 206 for a range request, got {}",
                response.status()
            ),
        });
    }

    let mut reader = response.into_reader();
    let mut buf = vec![0u8; READ_BUFFER];
    let mut offset = from;
    loop {
        if cancel.load(Ordering::Relaxed) || abort.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let n = reader.read(&mut buf).map_err(|e| DownloadError::Network {
            url: url.to_string(),
            message: e.to_string(),
        })?;
        if n == 0 {
            break;
        }
        write_all_at(file, &buf[..n], offset).map_err(|e| io_err(temp, e))?;
        offset += n as u64;
        done_chunk.fetch_add(n as u64, Ordering::Relaxed);
        downloaded.fetch_add(n as u64, Ordering::Relaxed);
    }

    let got = offset - from;
    let expected = end - from + 1;
    if got < expected {
        return Err(DownloadError::Network {
            url: url.to_string(),
            message: format!("range closed early: got {got} of {expected} bytes"),
        });
    }
    Ok(())
}

/// Positioned write that doesn't touch the shared file cursor, so several
/// workers can write disjoint ranges of the same file concurrently.
#[cfg(unix)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !buf.is_empty() {
        let n = file.write_at(buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "failed to write whole buffer",
            ));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_write(buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "failed to write whole buffer",
            ));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
compile_error!("cutlass-download's parallel writes require a unix or windows target");

/// The resume manifest: enough to validate a `*.part` still matches the remote
/// file and pick up each chunk where it stopped.
struct ResumeManifest {
    total: u64,
    chunk_size: u64,
    done: Vec<u64>,
}

/// Snapshot the live counters and write the manifest, logging (not failing) on
/// error — a lost manifest only costs re-downloaded bytes, never correctness.
fn persist_manifest(path: &Path, total: u64, chunk_size: u64, done: &[AtomicU64]) {
    let snapshot: Vec<u64> = done.iter().map(|d| d.load(Ordering::Relaxed)).collect();
    if let Err(e) = save_manifest(path, total, chunk_size, &snapshot) {
        warn!("could not persist resume manifest {}: {e}", path.display());
    }
}

fn save_manifest(path: &Path, total: u64, chunk_size: u64, done: &[u64]) -> std::io::Result<()> {
    let mut body = String::new();
    let _ = writeln!(body, "{MANIFEST_HEADER}");
    let _ = writeln!(body, "total {total}");
    let _ = writeln!(body, "chunk {chunk_size}");
    let joined = done
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let _ = writeln!(body, "done {joined}");

    // Write-then-rename so a crash mid-write can't leave a torn manifest.
    let tmp = append_suffix(path, ".tmp");
    fs::write(&tmp, body.as_bytes())?;
    fs::rename(&tmp, path)
}

fn load_manifest(path: &Path) -> Option<ResumeManifest> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != MANIFEST_HEADER {
        return None;
    }
    let mut total = None;
    let mut chunk_size = None;
    let mut done = None;
    for line in lines {
        if let Some(v) = line.strip_prefix("total ") {
            total = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("chunk ") {
            chunk_size = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("done ") {
            done = v
                .trim()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim().parse::<u64>().ok())
                .collect::<Option<Vec<u64>>>();
        }
    }
    Some(ResumeManifest {
        total: total?,
        chunk_size: chunk_size?,
        done: done?,
    })
}

/// A single transfer to the `*.part` temp file. Returns the streamed SHA-256
/// (lowercase hex) when the request carries a checksum, so the caller can
/// verify without re-reading the file.
fn fetch_once(
    agent: &ureq::Agent,
    opts: &DownloadOptions,
    req: &DownloadRequest,
    temp: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Option<String>, DownloadError> {
    let want_hash = req.checksum.is_some();

    let mut existing = if opts.resume {
        fs::metadata(temp).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Issue the request, translating a `416 Range Not Satisfiable` (our partial
    // is >= the remote size) into a clean restart from byte 0.
    let (response, append) = match request(agent, &req.url, existing) {
        Ok(response) => {
            if existing > 0 && response.status() == 206 {
                (response, true)
            } else {
                existing = 0;
                (response, false)
            }
        }
        Err(DownloadError::Http { status: 416, .. }) if existing > 0 => {
            existing = 0;
            (request(agent, &req.url, 0)?, false)
        }
        Err(err) => return Err(err),
    };

    let total = if append {
        content_range_total(response.header("Content-Range"))
    } else {
        parse_len(response.header("Content-Length"))
    };

    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(temp)
        .map_err(|e| io_err(temp, e))?;

    let mut hasher = want_hash.then(Sha256::new);

    if append {
        // Fold the bytes already on disk into the running hash, then position
        // the write cursor at the resume offset.
        if let Some(h) = hasher.as_mut() {
            file.seek(SeekFrom::Start(0)).map_err(|e| io_err(temp, e))?;
            hash_reader_into(&mut file, h).map_err(|e| io_err(temp, e))?;
        }
        file.seek(SeekFrom::Start(existing))
            .map_err(|e| io_err(temp, e))?;
    } else {
        file.set_len(0).map_err(|e| io_err(temp, e))?;
        file.seek(SeekFrom::Start(0)).map_err(|e| io_err(temp, e))?;
    }

    let mut downloaded = existing;
    let mut reader = response.into_reader();
    let mut buf = vec![0u8; READ_BUFFER];
    let mut last_emit = Instant::now();
    on_progress(Progress { downloaded, total });

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = file.flush();
            return Err(DownloadError::Cancelled);
        }
        let n = reader.read(&mut buf).map_err(|e| DownloadError::Network {
            url: req.url.clone(),
            message: e.to_string(),
        })?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| io_err(temp, e))?;
        if let Some(h) = hasher.as_mut() {
            h.update(&buf[..n]);
        }
        downloaded += n as u64;
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            on_progress(Progress { downloaded, total });
            last_emit = Instant::now();
        }
    }

    file.flush().map_err(|e| io_err(temp, e))?;
    // Best-effort durability before the rename publishes the file.
    let _ = file.sync_all();

    // A short read with a known total means the connection dropped early; keep
    // the partial for resume and report a retryable error.
    if let Some(total) = total {
        if downloaded < total {
            return Err(DownloadError::Network {
                url: req.url.clone(),
                message: format!("connection closed early: got {downloaded} of {total} bytes"),
            });
        }
    }

    on_progress(Progress { downloaded, total });
    Ok(hasher.map(|h| hex_lower(&h.finalize())))
}

/// GET `url`, optionally resuming from byte `from`, mapping `ureq`'s error
/// shape onto [`DownloadError`].
fn request(agent: &ureq::Agent, url: &str, from: u64) -> Result<ureq::Response, DownloadError> {
    let mut http = agent.get(url);
    if from > 0 {
        http = http.set("Range", &format!("bytes={from}-"));
    }
    match http.call() {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(status, _)) => Err(DownloadError::Http {
            url: url.to_string(),
            status,
        }),
        Err(ureq::Error::Transport(t)) => Err(DownloadError::Network {
            url: url.to_string(),
            message: t.to_string(),
        }),
    }
}

fn is_fatal_status(status: u16) -> bool {
    matches!(status, 400 | 401 | 403 | 404 | 405 | 410)
}

/// Parse a `Content-Length`-style numeric header.
fn parse_len(header: Option<&str>) -> Option<u64> {
    header.and_then(|s| s.trim().parse::<u64>().ok())
}

/// Pull the total size out of a `Content-Range: bytes START-END/TOTAL` header.
fn content_range_total(header: Option<&str>) -> Option<u64> {
    let total = header?.rsplit('/').next()?.trim();
    total.parse::<u64>().ok()
}

fn temp_path(dest: &Path) -> PathBuf {
    append_suffix(dest, ".part")
}

/// Append a literal suffix to a path's filename (so `foo.gguf` + `.part` is
/// `foo.gguf.part`, not a replaced extension).
fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Publish the temp file onto its final path. `rename` within a directory is
/// atomic, and the temp always sits beside the destination, so this never
/// crosses a filesystem boundary.
fn persist(temp: &Path, dest: &Path) -> Result<(), DownloadError> {
    fs::rename(temp, dest).map_err(|e| io_err(dest, e))
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    hash_reader_into(&mut file, &mut hasher)?;
    Ok(hex_lower(&hasher.finalize()))
}

fn hash_reader_into<R: Read>(reader: &mut R, hasher: &mut Sha256) -> std::io::Result<()> {
    let mut buf = vec![0u8; READ_BUFFER];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn io_err(path: &Path, source: std::io::Error) -> DownloadError {
    DownloadError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Sleep `initial_backoff * 2^attempt` (capped), in slices so cancellation is
/// observed promptly rather than after a full multi-second wait.
fn backoff(opts: &DownloadOptions, attempt: u32, cancel: &AtomicBool) -> Result<(), DownloadError> {
    let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
    let wait = opts.initial_backoff.saturating_mul(factor).min(MAX_BACKOFF);

    let slice = Duration::from_millis(50);
    let mut slept = Duration::ZERO;
    while slept < wait {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let step = slice.min(wait - slept);
        thread::sleep(step);
        slept += step;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_lower_pads_each_byte() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff, 0xa0]), "000fffa0");
        assert_eq!(hex_lower(&[]), "");
    }

    #[test]
    fn sha256_of_empty_input() {
        let mut hasher = Sha256::new();
        let digest = hex_lower(&hasher.finalize_reset());
        assert_eq!(
            digest,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn parse_len_handles_whitespace_and_garbage() {
        assert_eq!(parse_len(Some("1024")), Some(1024));
        assert_eq!(parse_len(Some("  42 ")), Some(42));
        assert_eq!(parse_len(Some("nan")), None);
        assert_eq!(parse_len(None), None);
    }

    #[test]
    fn content_range_total_extracts_size() {
        assert_eq!(content_range_total(Some("bytes 200-1023/1024")), Some(1024));
        assert_eq!(content_range_total(Some("bytes 0-0/1")), Some(1));
        // Unknown total ("*") and malformed values are simply absent.
        assert_eq!(content_range_total(Some("bytes 0-1/*")), None);
        assert_eq!(content_range_total(None), None);
    }

    #[test]
    fn fatal_statuses_are_not_retried() {
        assert!(is_fatal_status(404));
        assert!(is_fatal_status(403));
        assert!(!is_fatal_status(500));
        assert!(!is_fatal_status(429));
        assert!(!is_fatal_status(206));
    }

    #[test]
    fn temp_path_is_a_sibling_part_file() {
        assert_eq!(
            temp_path(Path::new("/models/m.gguf")),
            PathBuf::from("/models/m.gguf.part")
        );
    }

    #[test]
    fn checksum_compare_is_case_insensitive() {
        let c = Checksum::Sha256("ABCDEF".to_string());
        assert!(c.matches("abcdef"));
        assert!(!c.matches("abcde0"));
    }

    #[test]
    fn progress_fraction_guards_zero_and_unknown_totals() {
        assert_eq!(
            Progress {
                downloaded: 5,
                total: Some(10)
            }
            .fraction(),
            Some(0.5)
        );
        assert_eq!(
            Progress {
                downloaded: 5,
                total: None
            }
            .fraction(),
            None
        );
        assert_eq!(
            Progress {
                downloaded: 5,
                total: Some(0)
            }
            .fraction(),
            None
        );
    }
}
