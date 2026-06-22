//! End-to-end tests against a throwaway in-process HTTP/1.1 server.
//!
//! The server is intentionally tiny: it parses just enough of each request to
//! drive the behaviours the downloader cares about (status codes, `Range`,
//! abrupt disconnects) and serves one request per connection (every response
//! carries `Connection: close`).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cutlass_download::{DownloadError, DownloadOptions, DownloadRequest, Downloader};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Test HTTP server
// ---------------------------------------------------------------------------

struct Req {
    #[allow(dead_code)]
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Parse the start offset of a `Range: bytes=N-` header, if present.
    fn range_start(&self) -> Option<u64> {
        let rest = self.header("Range")?.trim().strip_prefix("bytes=")?;
        rest.split('-').next()?.trim().parse().ok()
    }
}

enum Action {
    Respond(Vec<u8>),
    /// Close the connection without replying (simulates a dropped transfer).
    Drop,
}

struct TestServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TestServer {
    fn start<H>(handler: H) -> Self
    where
        H: Fn(usize, &Req) -> Action + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);
        let count = Arc::new(AtomicUsize::new(0));

        let stop = shutdown.clone();
        let handle = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let handler = handler.clone();
                        let i = count.fetch_add(1, Ordering::Relaxed);
                        thread::spawn(move || serve_conn(stream, i, handler.as_ref()));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            shutdown,
            handle: Some(handle),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_conn<H>(mut stream: TcpStream, i: usize, handler: &H)
where
    H: Fn(usize, &Req) -> Action,
{
    stream.set_nonblocking(false).ok();
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    match handler(i, &req) {
        Action::Respond(bytes) => {
            let _ = stream.write_all(&bytes);
            let _ = stream.flush();
        }
        Action::Drop => {}
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Req> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut data = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&tmp[..n]);
                if find_subslice(&data, b"\r\n\r\n").is_some() || data.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    parse_request(&data)
}

fn parse_request(data: &[u8]) -> Option<Req> {
    let text = String::from_utf8_lossy(data);
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some(Req {
        method,
        path,
        headers,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Response builders
// ---------------------------------------------------------------------------

fn http_200(body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn http_206(full: &[u8], start: usize) -> Vec<u8> {
    let slice = &full[start..];
    let mut response = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
        slice.len(),
        start,
        full.len() - 1,
        full.len()
    )
    .into_bytes();
    response.extend_from_slice(slice);
    response
}

fn http_404() -> Vec<u8> {
    let body = b"not found";
    let mut response = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn http_head(len: usize, accept_ranges: bool) -> Vec<u8> {
    let ranges = if accept_ranges {
        "Accept-Ranges: bytes\r\n"
    } else {
        ""
    };
    format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n{ranges}Connection: close\r\n\r\n")
        .into_bytes()
}

fn http_status_only(status: u16) -> Vec<u8> {
    format!("HTTP/1.1 {status} Status\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .into_bytes()
}

fn http_206_range(full: &[u8], from: usize, end: usize) -> Vec<u8> {
    let slice = &full[from..=end];
    let mut response = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
        slice.len(),
        from,
        end,
        full.len()
    )
    .into_bytes();
    response.extend_from_slice(slice);
    response
}

/// Parse a `bytes=FROM-END` range; an absent END means "to the last byte".
fn parse_range(header: &str, len: usize) -> Option<(usize, usize)> {
    let rest = header.trim().strip_prefix("bytes=")?;
    let mut parts = rest.splitn(2, '-');
    let from: usize = parts.next()?.trim().parse().ok()?;
    let end = match parts.next().map(str::trim) {
        Some(s) if !s.is_empty() => s.parse().ok()?,
        _ => len - 1,
    };
    Some((from, end))
}

/// Records every served range and the running byte total, so a test can prove
/// resume avoided re-downloading finished chunks.
#[derive(Clone, Default)]
struct ServeLog {
    ranges: Arc<Mutex<Vec<(usize, usize)>>>,
    bytes: Arc<AtomicU64>,
}

/// How the server should answer a `HEAD`.
#[derive(Clone, Copy)]
enum HeadMode {
    /// Reply `200` with the size and (maybe) `Accept-Ranges: bytes`.
    Ok { accept_ranges: bool },
    /// Reply with a bare status (e.g. `405` to mean "HEAD unsupported").
    Status(u16),
}

/// A configurable file handler: control `HEAD` behaviour and whether `GET`
/// honours `Range` independently, so tests can model awkward real servers.
fn serve_file_ext(
    body: Arc<Vec<u8>>,
    head: HeadMode,
    honor_range: bool,
    log: ServeLog,
) -> impl Fn(usize, &Req) -> Action + Send + Sync + 'static {
    move |_, req| {
        if req.method == "HEAD" {
            return match head {
                HeadMode::Ok { accept_ranges } => {
                    Action::Respond(http_head(body.len(), accept_ranges))
                }
                HeadMode::Status(status) => Action::Respond(http_status_only(status)),
            };
        }
        if honor_range {
            if let Some((from, end)) = req.header("Range").and_then(|h| parse_range(h, body.len()))
            {
                log.ranges.lock().unwrap().push((from, end));
                log.bytes
                    .fetch_add((end - from + 1) as u64, Ordering::Relaxed);
                return Action::Respond(http_206_range(&body, from, end));
            }
        }
        log.bytes.fetch_add(body.len() as u64, Ordering::Relaxed);
        Action::Respond(http_200(&body))
    }
}

/// A handler that serves `body`, honouring ranges (and advertising them on
/// `HEAD`) iff `accept_ranges`.
fn serve_file(
    body: Arc<Vec<u8>>,
    accept_ranges: bool,
    log: ServeLog,
) -> impl Fn(usize, &Req) -> Action + Send + Sync + 'static {
    serve_file_ext(body, HeadMode::Ok { accept_ranges }, accept_ranges, log)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn test_opts() -> DownloadOptions {
    // Single connection: keeps the general-purpose tests on the single-stream
    // path (no HEAD probe, deterministic request counts).
    DownloadOptions {
        max_retries: 3,
        connect_timeout: Duration::from_secs(5),
        resume: true,
        user_agent: "cutlass-download-test".to_string(),
        initial_backoff: Duration::from_millis(5),
        max_connections: 1,
        min_parallel_size: 8 * 1024 * 1024,
        chunk_size: 8 * 1024 * 1024,
        probe_timeout: Duration::from_secs(5),
    }
}

fn parallel_opts() -> DownloadOptions {
    // Tiny thresholds so a modest test body splits into many chunks.
    DownloadOptions {
        max_connections: 4,
        min_parallel_size: 4096,
        chunk_size: 4096,
        ..test_opts()
    }
}

fn body_of(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_add(seed)).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn happy_path_writes_and_verifies() {
    let body = Arc::new(body_of(50_000, 7));
    let expected = sha256_hex(&body);
    let served = body.clone();
    let server = TestServer::start(move |_, _| Action::Respond(http_200(&served)));

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.bin");
    let temp = dir.path().join("file.bin.part");

    let dl = Downloader::new(test_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/file.bin"), &dest).with_sha256(&expected);

    let mut seen_progress = false;
    let out = dl
        .fetch(&req, &cancel, &mut |_| seen_progress = true)
        .unwrap();

    assert_eq!(out, dest);
    assert_eq!(std::fs::read(&dest).unwrap(), *body);
    assert!(
        !temp.exists(),
        "temp file should be renamed away on success"
    );
    assert!(seen_progress, "progress callback should fire at least once");
}

#[test]
fn resume_continues_from_existing_part() {
    let body = Arc::new(body_of(40_000, 3));
    let expected = sha256_hex(&body);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("f.bin");
    let temp = dir.path().join("f.bin.part");

    // Pre-seed a half-finished transfer.
    let half = 15_000usize;
    std::fs::write(&temp, &body[..half]).unwrap();

    let served = body.clone();
    let saw_range = Arc::new(AtomicBool::new(false));
    let flag = saw_range.clone();
    let server = TestServer::start(move |_, req| {
        if let Some(start) = req.range_start() {
            flag.store(true, Ordering::Relaxed);
            Action::Respond(http_206(&served, start as usize))
        } else {
            Action::Respond(http_200(&served))
        }
    });

    let dl = Downloader::new(test_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/f.bin"), &dest).with_sha256(&expected);
    dl.fetch(&req, &cancel, &mut |_| {}).unwrap();

    assert!(
        saw_range.load(Ordering::Relaxed),
        "server should have received a Range request"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), *body);
}

#[test]
fn retries_after_dropped_connection() {
    let body = Arc::new(body_of(20_000, 1));
    let served = body.clone();
    let server = TestServer::start(move |i, _| {
        if i == 0 {
            Action::Drop
        } else {
            Action::Respond(http_200(&served))
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("retry.bin");
    let dl = Downloader::new(test_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/retry.bin"), &dest);

    dl.fetch(&req, &cancel, &mut |_| {}).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), *body);
}

#[test]
fn checksum_mismatch_is_reported_and_partial_discarded() {
    let body = Arc::new(b"the actual bytes".to_vec());
    let served = body.clone();
    let server = TestServer::start(move |_, _| Action::Respond(http_200(&served)));

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("bad.bin");
    let temp = dir.path().join("bad.bin.part");

    let opts = DownloadOptions {
        max_retries: 0,
        ..test_opts()
    };
    let dl = Downloader::new(opts);
    let cancel = AtomicBool::new(false);
    let wrong = sha256_hex(b"something else entirely");
    let req = DownloadRequest::new(server.url("/bad.bin"), &dest).with_sha256(&wrong);

    let err = dl.fetch(&req, &cancel, &mut |_| {}).unwrap_err();
    assert!(
        matches!(err, DownloadError::Checksum { .. }),
        "expected checksum error, got {err:?}"
    );
    assert!(!dest.exists(), "a failed verify must not publish the file");
    assert!(!temp.exists(), "the bad partial must be discarded");
}

#[test]
fn fatal_404_is_not_retried() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let server = TestServer::start(move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Action::Respond(http_404())
    });

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("missing.bin");
    let dl = Downloader::new(test_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/missing.bin"), &dest);

    let err = dl.fetch(&req, &cancel, &mut |_| {}).unwrap_err();
    match err {
        DownloadError::Http { status, .. } => assert_eq!(status, 404),
        other => panic!("expected HTTP 404, got {other:?}"),
    }
    assert_eq!(
        hits.load(Ordering::Relaxed),
        1,
        "a 404 must be requested exactly once"
    );
}

#[test]
fn already_present_and_valid_is_a_cache_hit() {
    let body = Arc::new(body_of(8_000, 9));
    let expected = sha256_hex(&body);
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let server = TestServer::start(move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Action::Respond(http_404())
    });

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("cached.bin");
    std::fs::write(&dest, &*body).unwrap();

    let dl = Downloader::new(test_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/cached.bin"), &dest).with_sha256(&expected);

    let out = dl.fetch(&req, &cancel, &mut |_| {}).unwrap();
    assert_eq!(out, dest);
    assert_eq!(
        hits.load(Ordering::Relaxed),
        0,
        "a valid existing file must not hit the network"
    );
}

#[test]
fn fetch_many_downloads_every_file() {
    let bodies: Arc<Vec<Vec<u8>>> =
        Arc::new((0..6).map(|i| body_of(2_000 + i * 700, i as u8)).collect());
    let served = bodies.clone();
    let server = TestServer::start(move |_, req| {
        let idx: usize = req.path.trim_start_matches("/f").parse().unwrap();
        Action::Respond(http_200(&served[idx]))
    });

    let dir = tempfile::tempdir().unwrap();
    let dl = Downloader::new(test_opts());
    let cancel = AtomicBool::new(false);

    let reqs: Vec<DownloadRequest> = (0..bodies.len())
        .map(|i| {
            DownloadRequest::new(
                server.url(&format!("/f{i}")),
                dir.path().join(format!("f{i}.bin")),
            )
            .with_sha256(sha256_hex(&bodies[i]))
        })
        .collect();

    let results = dl.fetch_many(&reqs, 3, &cancel, &|_, _| {});
    assert_eq!(results.len(), bodies.len());
    for (i, result) in results.iter().enumerate() {
        let path = result
            .as_ref()
            .unwrap_or_else(|e| panic!("file {i} failed: {e:?}"));
        assert_eq!(std::fs::read(path).unwrap(), bodies[i]);
    }
}

#[test]
fn cancellation_stops_before_fetching() {
    let server = TestServer::start(move |_, _| Action::Respond(http_200(b"unused")));
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("cancelled.bin");
    let dl = Downloader::new(test_opts());
    let cancel = AtomicBool::new(true);
    let req = DownloadRequest::new(server.url("/x"), &dest);

    let err = dl.fetch(&req, &cancel, &mut |_| {}).unwrap_err();
    assert!(matches!(err, DownloadError::Cancelled));
    assert!(!dest.exists());
}

#[test]
fn parallel_assembles_file_from_chunks() {
    let body = Arc::new(body_of(50_000, 5));
    let expected = sha256_hex(&body);
    let log = ServeLog::default();
    let server = TestServer::start(serve_file(body.clone(), true, log.clone()));

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("big.bin");
    let temp = dir.path().join("big.bin.part");

    let dl = Downloader::new(parallel_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/big.bin"), &dest).with_sha256(&expected);

    let mut last = 0u64;
    dl.fetch(&req, &cancel, &mut |p| last = p.downloaded)
        .unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), *body);
    assert_eq!(last, body.len() as u64, "final progress should reach total");
    assert!(!temp.exists(), "temp should be published on success");

    // 50_000 / 4096 -> 13 chunks, each requested exactly once.
    let ranges = log.ranges.lock().unwrap();
    assert_eq!(ranges.len(), 13, "expected one request per chunk");
    let served: usize = ranges.iter().map(|&(from, end)| end - from + 1).sum();
    assert_eq!(served, body.len(), "chunks should tile the whole file");
}

#[test]
fn parallel_falls_back_when_ranges_unsupported() {
    let body = Arc::new(body_of(50_000, 2));
    let expected = sha256_hex(&body);
    let log = ServeLog::default();
    // accept_ranges = false -> HEAD advertises no ranges -> single stream.
    let server = TestServer::start(serve_file(body.clone(), false, log.clone()));

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("nospan.bin");
    let dl = Downloader::new(parallel_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/nospan.bin"), &dest).with_sha256(&expected);

    dl.fetch(&req, &cancel, &mut |_| {}).unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), *body);
    assert!(
        log.ranges.lock().unwrap().is_empty(),
        "no range requests should be made when the server can't do them"
    );
}

#[test]
fn parallel_resume_skips_completed_chunks() {
    let chunk = 4096usize;
    let body = Arc::new(body_of(40_000, 8));
    let expected = sha256_hex(&body);

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("r.bin");
    let temp = dir.path().join("r.bin.part");
    let manifest = dir.path().join("r.bin.part.progress");

    // Pre-seed chunk 0 on disk; the downloader's set_len extends the rest.
    std::fs::write(&temp, &body[..chunk]).unwrap();

    // Hand-write the resume manifest (mirrors the crate's text format) marking
    // chunk 0 as already complete.
    let num_chunks = body.len().div_ceil(chunk);
    let mut done = vec![0u64; num_chunks];
    done[0] = chunk as u64;
    let done_csv = done
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    std::fs::write(
        &manifest,
        format!(
            "cutlass-download-resume v1\ntotal {}\nchunk {}\ndone {}\n",
            body.len(),
            chunk,
            done_csv
        ),
    )
    .unwrap();

    let log = ServeLog::default();
    let server = TestServer::start(serve_file(body.clone(), true, log.clone()));

    let dl = Downloader::new(parallel_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/r.bin"), &dest).with_sha256(&expected);
    dl.fetch(&req, &cancel, &mut |_| {}).unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), *body);
    let ranges = log.ranges.lock().unwrap();
    assert!(
        !ranges.iter().any(|&(from, _)| from == 0),
        "the completed first chunk must not be refetched"
    );
    assert_eq!(
        log.bytes.load(Ordering::Relaxed),
        (body.len() - chunk) as u64,
        "only the unfinished chunks should be served"
    );
    assert!(
        !manifest.exists(),
        "manifest is removed once the file is whole"
    );
}

#[test]
fn parallel_works_when_head_unsupported() {
    // Server rejects HEAD (405) but honours Range on GET — the case where the
    // HEAD-only probe would have wrongly given up on parallelism.
    let body = Arc::new(body_of(50_000, 6));
    let expected = sha256_hex(&body);
    let log = ServeLog::default();
    let server = TestServer::start(serve_file_ext(
        body.clone(),
        HeadMode::Status(405),
        true,
        log.clone(),
    ));

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("nohead.bin");
    let dl = Downloader::new(parallel_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/nohead.bin"), &dest).with_sha256(&expected);
    dl.fetch(&req, &cancel, &mut |_| {}).unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), *body);
    let ranges = log.ranges.lock().unwrap();
    assert!(
        ranges.contains(&(0, 0)),
        "should settle range support with a 1-byte GET probe"
    );
    // The probe plus one request per chunk (50_000 / 4096 -> 13).
    assert_eq!(ranges.len(), 14, "expected 1 probe + 13 chunk requests");
}

#[test]
fn parallel_falls_back_when_range_probe_is_ignored() {
    // The dangerous server: no HEAD, and it ignores Range on GET (answers 200
    // with the whole body). The probe must detect this from the status alone
    // and fall back to a single stream rather than downloading the file twice.
    let body = Arc::new(body_of(50_000, 4));
    let expected = sha256_hex(&body);
    let log = ServeLog::default();
    let server = TestServer::start(serve_file_ext(
        body.clone(),
        HeadMode::Status(405),
        false,
        log.clone(),
    ));

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("ignored.bin");
    let dl = Downloader::new(parallel_opts());
    let cancel = AtomicBool::new(false);
    let req = DownloadRequest::new(server.url("/ignored.bin"), &dest).with_sha256(&expected);
    dl.fetch(&req, &cancel, &mut |_| {}).unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), *body);
    assert!(
        log.ranges.lock().unwrap().is_empty(),
        "a range-ignoring server must never be driven with parallel range requests"
    );
}
