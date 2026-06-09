//! Disk-backed YUV frame cache for accurate-seek scrub acceleration.
//!
//! Model:
//!   - One append-only blob per source: `cache/{source_id}.yuv` (dumb bytes).
//!   - An in-RAM index (PTS -> offset/len/last_access), persisted to a sidecar
//!     manifest so the cache survives app restart (warm second session).
//!   - Writes happen on a dedicated writer thread, fed over a crossbeam channel
//!     from the decode thread. The decode thread's job ENDS at `send()`.
//!   - Reads are mmap slices (OS page cache = automatic RAM tier).
//!   - Global byte budget, batch LRU eviction across sources.
//!
//! Invariant you cannot break: BLOB WRITE COMMITS BEFORE THE INDEX ENTRY.
//! Orphan bytes (blob written, index not) are harmless dead weight on disk until
//! compaction (not implemented yet). The reverse (index points at bytes that
//! aren't there) serves a corrupt frame at the playhead. Never do the reverse.
//!
//! Blob writes use explicit `seek` + `write_all` at `write_cursor` — never
//! `O_APPEND`, so recorded offsets always match physical byte locations.
//!
//! LRU eviction is logical only: it drops index entries and lowers
//! [`FrameCache::total_bytes`], but does not shrink `.yuv` blobs on disk.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, bounded};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// `ENOSPC` on Unix; used when building synthetic errors in tests.
#[cfg(unix)]
const ERR_NO_SPACE: i32 = 28;
#[cfg(windows)]
const ERR_NO_SPACE: i32 = 112;

#[cfg(test)]
fn synthetic_no_space_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(ERR_NO_SPACE)
}

fn is_no_space(err: &std::io::Error) -> bool {
    err.kind() == ErrorKind::StorageFull || err.raw_os_error() == Some(ERR_NO_SPACE)
}

// ----------------------------------------------------------------------------
// Keys / fingerprints
// ----------------------------------------------------------------------------

pub type SourceId = u64;

/// Identifies a source file. Fast check is `size + mtime`. If you hit phantom
/// invalidations from cloud sync touching mtime, fall back to hashing
/// first+last 64KB before declaring a source stale (see `register_source`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceFingerprint {
    pub path: String,
    pub size: u64,
    pub mtime_ns: u128,
}

impl SourceFingerprint {
    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        let meta = fs::metadata(path)?;
        let mtime_ns = meta
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            size: meta.len(),
            mtime_ns,
        })
    }

    /// Stable id used as the blob filename. Hash of (path, size, mtime).
    pub fn id(&self) -> SourceId {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.path.hash(&mut h);
        self.size.hash(&mut h);
        self.mtime_ns.hash(&mut h);
        h.finish()
    }
}

/// What we cache a given source AS. The cache is res/pixfmt-specific; a source
/// re-cached at a different res is effectively a different blob.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheSpec {
    pub width: u32,
    pub height: u32,
    pub pixfmt: String, // e.g. "yuv420p", stored PACKED (pad rows to 256 on load)
}

// ----------------------------------------------------------------------------
// Index entry + manifest (the "DB" — frames never live here, only pointers)
// ----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct FrameEntry {
    pub offset: u64,
    pub len: u32,
    pub last_access: u64, // monotonic counter from CacheState.access_counter
}

#[derive(Serialize, Deserialize)]
pub struct SourceManifest {
    pub fingerprint: SourceFingerprint,
    pub spec: CacheSpec,
    pub write_cursor: u64,               // next write offset in the blob
    pub index: HashMap<i64, FrameEntry>, // pts -> entry
}

// ----------------------------------------------------------------------------
// Per-source cache: one blob file, lazily (re)mapped for reads
// ----------------------------------------------------------------------------

struct SourceCache {
    manifest: SourceManifest,
    blob_path: PathBuf,
    manifest_path: PathBuf,
    writer: File, // read+write handle; writes always seek to write_cursor first
    reader: Option<Mmap>,
    mapped_len: u64, // how much of the blob the current mmap covers
    bytes_used: u64, // live bytes (== sum of entry.len; not file size if fragmented)
    #[cfg(test)]
    fail_writes_enospc: Arc<AtomicBool>,
    #[cfg(test)]
    fail_writes_after_bytes: Arc<AtomicUsize>,
    #[cfg(test)]
    fail_flush_enospc: Arc<AtomicBool>,
}

impl SourceCache {
    fn open_or_create(
        cache_dir: &Path,
        id: SourceId,
        fingerprint: SourceFingerprint,
        spec: CacheSpec,
        #[cfg(test)] fail_writes_enospc: Arc<AtomicBool>,
        #[cfg(test)] fail_writes_after_bytes: Arc<AtomicUsize>,
        #[cfg(test)] fail_flush_enospc: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let blob_path = cache_dir.join(format!("{id:016x}.yuv"));
        let manifest_path = cache_dir.join(format!("{id:016x}.idx"));

        let loaded = match File::open(&manifest_path) {
            Ok(mut f) => {
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                bincode::deserialize::<SourceManifest>(&buf).ok()
            }
            Err(_) => None,
        };

        let manifest = match loaded {
            Some(m) if m.fingerprint == fingerprint && m.spec == spec => m,
            // Stale spec or fingerprint: drop the index; prior blob bytes become orphans.
            Some(_) | None => SourceManifest {
                fingerprint,
                spec,
                write_cursor: 0,
                index: HashMap::new(),
            },
        };

        let writer = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&blob_path)?;

        let mut manifest = manifest;
        let file_len = writer.metadata()?.len();
        if manifest.index.is_empty() {
            // Fresh or invalidated manifest: append after any existing blob tail.
            manifest.write_cursor = file_len;
        } else if manifest.write_cursor > file_len {
            manifest.write_cursor = file_len;
            manifest.index.clear();
        } else if manifest.write_cursor < file_len {
            // Crash left orphan bytes after the cursor; skip them for new appends.
            manifest.write_cursor = file_len;
        }

        let bytes_used = manifest.index.values().map(|e| e.len as u64).sum();

        Ok(Self {
            manifest,
            blob_path,
            manifest_path,
            writer,
            reader: None,
            mapped_len: 0,
            bytes_used,
            #[cfg(test)]
            fail_writes_enospc,
            #[cfg(test)]
            fail_writes_after_bytes,
            #[cfg(test)]
            fail_flush_enospc,
        })
    }

    fn matches(&self, fingerprint: &SourceFingerprint, spec: &CacheSpec) -> bool {
        self.manifest.fingerprint == *fingerprint && self.manifest.spec == *spec
    }

    fn resync_write_cursor(&mut self) -> std::io::Result<()> {
        self.manifest.write_cursor = self.writer.metadata()?.len();
        Ok(())
    }

    /// Write bytes at `write_cursor` without advancing it. Resyncs cursor on error.
    fn write_blob_at_cursor(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fail_writes_enospc.load(Ordering::Acquire) {
            return Err(synthetic_no_space_error());
        }

        self.writer
            .seek(SeekFrom::Start(self.manifest.write_cursor))?;

        #[cfg(test)]
        {
            let fail_after = self.fail_writes_after_bytes.load(Ordering::Acquire);
            if fail_after > 0 {
                let n = fail_after.min(bytes.len());
                if n > 0 {
                    self.writer.write_all(&bytes[..n])?;
                }
                self.resync_write_cursor()?;
                return Err(synthetic_no_space_error());
            }
        }

        if let Err(e) = self.writer.write_all(bytes) {
            let _ = self.resync_write_cursor();
            return Err(e);
        }
        Ok(())
    }

    /// BLOB FIRST, INDEX SECOND. See module docs.
    fn append_frame(&mut self, pts: i64, bytes: &[u8], access: u64) -> std::io::Result<u64> {
        let offset = self.manifest.write_cursor;
        // 1. blob
        self.write_blob_at_cursor(bytes)?;
        self.manifest.write_cursor += bytes.len() as u64;

        // 2. index
        self.manifest.index.insert(
            pts,
            FrameEntry {
                offset,
                len: bytes.len() as u32,
                last_access: access,
            },
        );
        self.bytes_used += bytes.len() as u64;
        Ok(bytes.len() as u64)
    }

    /// Refresh the mmap when the blob grew past what we've mapped.
    fn remap_if_needed(&mut self) -> std::io::Result<()> {
        if self.reader.is_none() || self.mapped_len < self.manifest.write_cursor {
            self.writer.flush()?; // make appended bytes visible to a fresh map
            let f = File::open(&self.blob_path)?;
            let len = f.metadata()?.len();
            if len > 0 {
                // SAFETY: append-only blob; we never shrink under a live map.
                self.reader = Some(unsafe { Mmap::map(&f)? });
                self.mapped_len = len;
            }
        }
        Ok(())
    }

    fn get(&mut self, pts: i64, access: u64) -> Option<Vec<u8>> {
        let entry = *self.manifest.index.get(&pts)?;
        self.remap_if_needed().ok()?;
        let mmap = self.reader.as_ref()?;
        let start = entry.offset as usize;
        let end = start + entry.len as usize;
        let slice = mmap.get(start..end)?;

        if let Some(e) = self.manifest.index.get_mut(&pts) {
            e.last_access = access;
        }

        Some(slice.to_vec())
    }

    fn flush_manifest(&self) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fail_flush_enospc.load(Ordering::Acquire) {
            return Err(synthetic_no_space_error());
        }

        let buf = bincode::serialize(&self.manifest)
            .map_err(std::io::Error::other)?;
        let tmp = self.manifest_path.with_extension("idx.tmp");
        fs::write(&tmp, &buf)?;
        fs::rename(&tmp, &self.manifest_path)?;
        Ok(())
    }
}

// ----------------------------------------------------------------------------
// Global cache: owns all sources, the writer thread, the budget
// ----------------------------------------------------------------------------

/// Message from decode thread -> writer thread. Decode thread is done after send.
pub struct WriteMsg {
    pub source_id: SourceId,
    pub pts: i64,
    pub bytes: Vec<u8>, // packed YUV for one frame at the source's cache spec
}

enum WriterCmd {
    Write(WriteMsg),
    /// Block until all prior writes are persisted and manifests flushed.
    Sync(Sender<()>),
}

struct CacheState {
    cache_dir: PathBuf,
    sources: HashMap<SourceId, SourceCache>,
    total_bytes: u64,
    budget_bytes: u64,
    access_counter: u64,
    writes_since_flush: u32,
    #[cfg(test)]
    fail_writes_enospc: Arc<AtomicBool>,
    #[cfg(test)]
    fail_writes_after_bytes: Arc<AtomicUsize>,
    #[cfg(test)]
    fail_flush_enospc: Arc<AtomicBool>,
}

const DEFAULT_CHANNEL_CAPACITY: usize = 256;
const FLUSH_EVERY_N_WRITES: u32 = 64;

struct FrameCacheInner {
    state: Arc<Mutex<CacheState>>,
    disk_pressure: Arc<AtomicBool>,
    tx: Option<Sender<WriterCmd>>,
    writer: Option<JoinHandle<()>>,
}

impl Drop for FrameCacheInner {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(writer) = self.writer.take()
            && writer.join().is_err()
        {
            warn!("frame-cache writer thread panicked during shutdown");
        }
    }
}

#[derive(Clone)]
pub struct FrameCache {
    inner: Arc<FrameCacheInner>,
}

impl FrameCache {
    pub fn new(cache_dir: PathBuf, budget_bytes: u64) -> std::io::Result<Self> {
        Self::with_channel_capacity(cache_dir, budget_bytes, DEFAULT_CHANNEL_CAPACITY)
    }

    fn with_channel_capacity(
        cache_dir: PathBuf,
        budget_bytes: u64,
        channel_capacity: usize,
    ) -> std::io::Result<Self> {
        fs::create_dir_all(&cache_dir)?;
        let state = Arc::new(Mutex::new(CacheState {
            cache_dir,
            sources: HashMap::new(),
            total_bytes: 0,
            budget_bytes,
            access_counter: 0,
            writes_since_flush: 0,
            #[cfg(test)]
            fail_writes_enospc: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_writes_after_bytes: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            fail_flush_enospc: Arc::new(AtomicBool::new(false)),
        }));
        let disk_pressure = Arc::new(AtomicBool::new(false));

        let (tx, rx): (Sender<WriterCmd>, Receiver<WriterCmd>) = bounded(channel_capacity);
        let writer_state = Arc::clone(&state);
        let writer_pressure = Arc::clone(&disk_pressure);
        let writer = thread::Builder::new()
            .name("frame-cache-writer".into())
            .spawn(move || writer_loop(rx, writer_state, writer_pressure))
            .expect("spawn frame-cache writer");

        Ok(Self {
            inner: Arc::new(FrameCacheInner {
                state,
                disk_pressure,
                tx: Some(tx),
                writer: Some(writer),
            }),
        })
    }

    fn tx(&self) -> &Sender<WriterCmd> {
        self.inner
            .tx
            .as_ref()
            .expect("frame cache writer shut down")
    }

    /// Register a source before caching/reading from it. Opens/creates its blob,
    /// reconciles fingerprint (lazy invalidation), returns its id.
    pub fn register_source(
        &self,
        fingerprint: SourceFingerprint,
        spec: CacheSpec,
    ) -> std::io::Result<SourceId> {
        let id = fingerprint.id();
        let mut st = self.inner.state.lock().unwrap();
        let cache_dir = st.cache_dir.clone();

        if let Some(existing) = st.sources.get(&id) {
            if existing.matches(&fingerprint, &spec) {
                return Ok(id);
            }
            st.total_bytes = st.total_bytes.saturating_sub(existing.bytes_used);
            st.sources.remove(&id);
        }

        let sc = SourceCache::open_or_create(
            &cache_dir,
            id,
            fingerprint,
            spec,
            #[cfg(test)]
            Arc::clone(&st.fail_writes_enospc),
            #[cfg(test)]
            Arc::clone(&st.fail_writes_after_bytes),
            #[cfg(test)]
            Arc::clone(&st.fail_flush_enospc),
        )?;
        st.total_bytes += sc.bytes_used;
        st.sources.insert(id, sc);
        Ok(id)
    }

    /// Test-only: simulate `ENOSPC` on subsequent blob writes for this cache instance.
    #[cfg(test)]
    pub fn set_fail_writes_enospc_for_test(&self, enabled: bool) {
        self.inner
            .state
            .lock()
            .unwrap()
            .fail_writes_enospc
            .store(enabled, Ordering::Release);
    }

    /// Test-only: write `n` bytes then fail with `ENOSPC` on the next blob write.
    #[cfg(test)]
    pub fn set_fail_writes_after_bytes_for_test(&self, n: usize) {
        self.inner
            .state
            .lock()
            .unwrap()
            .fail_writes_after_bytes
            .store(n, Ordering::Release);
    }

    /// Test-only: simulate `ENOSPC` on subsequent manifest flushes for this cache instance.
    #[cfg(test)]
    pub fn set_fail_flush_enospc_for_test(&self, enabled: bool) {
        self.inner
            .state
            .lock()
            .unwrap()
            .fail_flush_enospc
            .store(enabled, Ordering::Release);
    }

    /// READ PATH. Hit -> Some(bytes). Miss -> None (caller decodes, then `cache_frame`).
    pub fn get(&self, source_id: SourceId, pts: i64) -> Option<Vec<u8>> {
        let mut st = self.inner.state.lock().unwrap();
        st.access_counter += 1;
        let access = st.access_counter;
        st.sources.get_mut(&source_id)?.get(pts, access)
    }

    /// WRITE PATH, called from the DECODE THREAD on a settle seek, once per
    /// intermediate frame. Non-blocking: if the writer is backed up we drop the
    /// frame rather than stall decode. No-ops while [`Self::disk_pressure`] is set.
    pub fn cache_frame(&self, source_id: SourceId, pts: i64, bytes: Vec<u8>) {
        if self.inner.disk_pressure.load(Ordering::Acquire) {
            return;
        }
        let _ = self.tx().try_send(WriterCmd::Write(WriteMsg {
            source_id,
            pts,
            bytes,
        }));
    }

    /// True after a no-space I/O error until the next successful write or
    /// [`Self::clear_disk_pressure`].
    pub fn disk_pressure(&self) -> bool {
        self.inner.disk_pressure.load(Ordering::Acquire)
    }

    /// Allow cache writes to resume after the user frees disk space.
    pub fn clear_disk_pressure(&self) {
        self.inner
            .disk_pressure
            .store(false, Ordering::Release);
    }

    /// Block until all queued writes are applied and manifests are flushed.
    /// Intended for tests and session shutdown — not for the decode hot path.
    pub fn sync(&self) {
        let (done_tx, done_rx) = bounded(1);
        if self.tx().send(WriterCmd::Sync(done_tx)).is_ok() {
            let _ = done_rx.recv();
        }
    }

    /// Live indexed bytes across all sources (logical budget; not on-disk blob size).
    pub fn total_bytes(&self) -> u64 {
        self.inner.state.lock().unwrap().total_bytes
    }

    /// Number of indexed frames for a registered source.
    pub fn frame_count(&self, source_id: SourceId) -> usize {
        self.inner
            .state
            .lock()
            .unwrap()
            .sources
            .get(&source_id)
            .map(|sc| sc.manifest.index.len())
            .unwrap_or(0)
    }

    /// Whether a PTS is present in the index (does not read blob bytes).
    pub fn contains(&self, source_id: SourceId, pts: i64) -> bool {
        self.inner
            .state
            .lock()
            .unwrap()
            .sources
            .get(&source_id)
            .is_some_and(|sc| sc.manifest.index.contains_key(&pts))
    }
}

fn writer_loop(
    rx: Receiver<WriterCmd>,
    state: Arc<Mutex<CacheState>>,
    disk_pressure: Arc<AtomicBool>,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            WriterCmd::Write(msg) => handle_write(msg, &state, &disk_pressure),
            WriterCmd::Sync(done) => {
                let mut st = state.lock().unwrap();
                flush_all(&mut st, &disk_pressure);
                st.writes_since_flush = 0;
                let _ = done.send(());
            }
        }
    }
    let mut st = state.lock().unwrap();
    flush_all(&mut st, &disk_pressure);
}

fn handle_write(msg: WriteMsg, state: &Arc<Mutex<CacheState>>, disk_pressure: &AtomicBool) {
    let mut st = state.lock().unwrap();
    st.access_counter += 1;
    let access = st.access_counter;

    let mut appended = false;
    if st.sources.get(&msg.source_id).is_some_and(|sc| sc.manifest.index.contains_key(&msg.pts)) {
        // Already cached.
    } else if let Some(written) =
        try_append_frame(&mut st, msg.source_id, msg.pts, &msg.bytes, access, disk_pressure)
    {
        st.total_bytes += written;
        appended = true;
    }

    if !appended {
        return;
    }

    st.writes_since_flush += 1;
    if st.writes_since_flush >= FLUSH_EVERY_N_WRITES {
        flush_all(&mut st, disk_pressure);
        st.writes_since_flush = 0;
    }

    if st.total_bytes > st.budget_bytes {
        evict(&mut st);
    }
}

fn try_append_frame(
    st: &mut CacheState,
    source_id: SourceId,
    pts: i64,
    bytes: &[u8],
    access: u64,
    disk_pressure: &AtomicBool,
) -> Option<u64> {
    let sc = st.sources.get_mut(&source_id)?;

    match sc.append_frame(pts, bytes, access) {
        Ok(written) => {
            disk_pressure.store(false, Ordering::Release);
            Some(written)
        }
        Err(e) if is_no_space(&e) => {
            warn!(
                source_id,
                pts,
                error = %e,
                "frame-cache blob write hit disk full; pausing writes"
            );
            // Logical LRU eviction does not reclaim append-only blob bytes on disk,
            // so evicting here would only destroy warm cache entries without helping.
            disk_pressure.store(true, Ordering::Release);
            None
        }
        Err(e) => {
            warn!(source_id, pts, error = %e, "frame-cache append failed");
            None
        }
    }
}

fn flush_all(st: &mut CacheState, disk_pressure: &AtomicBool) {
    let mut saw_no_space = false;
    for sc in st.sources.values() {
        if let Err(e) = sc.flush_manifest() {
            if is_no_space(&e) {
                warn!(error = %e, "frame-cache manifest flush hit disk full; pausing writes");
                saw_no_space = true;
            } else {
                warn!(error = %e, "frame-cache manifest flush failed");
            }
        }
    }
    if saw_no_space {
        disk_pressure.store(true, Ordering::Release);
    }
}

const EVICT_BATCH_FRACTION: f64 = 0.10;

fn evict(st: &mut CacheState) {
    evict_with_fraction(st, EVICT_BATCH_FRACTION, st.budget_bytes);
}

fn evict_with_fraction(st: &mut CacheState, fraction: f64, budget_target: u64) {
    let mut all: Vec<(u64, SourceId, i64, u32)> = Vec::new();
    for (sid, sc) in st.sources.iter() {
        for (&pts, e) in sc.manifest.index.iter() {
            all.push((e.last_access, *sid, pts, e.len));
        }
    }
    all.sort_by_key(|(access, ..)| *access);

    let target = (st.total_bytes as f64 * fraction) as u64;
    let mut freed = 0u64;

    for (_acc, sid, pts, len) in all {
        if freed >= target && st.total_bytes.saturating_sub(freed) <= budget_target {
            break;
        }
        if let Some(sc) = st.sources.get_mut(&sid)
            && sc.manifest.index.remove(&pts).is_some()
        {
            sc.bytes_used = sc.bytes_used.saturating_sub(len as u64);
            freed += len as u64;
        }
    }
    st.total_bytes = st.total_bytes.saturating_sub(freed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SOURCE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_fingerprint(path_suffix: &str) -> SourceFingerprint {
        let n = SOURCE_COUNTER.fetch_add(1, Ordering::Relaxed);
        SourceFingerprint {
            path: format!("/virtual/source_{n}_{path_suffix}"),
            size: 1_000 + n,
            mtime_ns: 1_700_000_000_000_000_000 + u128::from(n),
        }
    }

    fn test_spec() -> CacheSpec {
        CacheSpec {
            width: 1920,
            height: 1080,
            pixfmt: "yuv420p".into(),
        }
    }

    fn frame_bytes(pts: i64, len: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; len];
        bytes[0] = (pts & 0xff) as u8;
        bytes[1] = ((pts >> 8) & 0xff) as u8;
        bytes
    }

    fn open_cache(dir: &Path, budget: u64) -> FrameCache {
        FrameCache::new(dir.to_path_buf(), budget).expect("open cache")
    }

    fn register(cache: &FrameCache, fp: &SourceFingerprint) -> SourceId {
        cache
            .register_source(fp.clone(), test_spec())
            .expect("register source")
    }

    fn cache_and_sync(cache: &FrameCache, source_id: SourceId, pts: i64, bytes: Vec<u8>) {
        cache.cache_frame(source_id, pts, bytes);
        cache.sync();
    }

    #[test]
    fn roundtrip_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("roundtrip");
        let source_id = register(&cache, &fp);

        let bytes = frame_bytes(100, 256);
        cache_and_sync(&cache, source_id, 100, bytes.clone());

        assert_eq!(cache.get(source_id, 100), Some(bytes));
        assert_eq!(cache.frame_count(source_id), 1);
        assert_eq!(cache.total_bytes(), 256);
    }

    #[test]
    fn get_returns_none_on_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("miss");
        let source_id = register(&cache, &fp);

        assert!(cache.get(source_id, 42).is_none());
        assert!(!cache.contains(source_id, 42));
    }

    #[test]
    fn unregistered_source_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        assert!(cache.get(0xdead_beef, 1).is_none());
    }

    #[test]
    fn duplicate_pts_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("dup");
        let source_id = register(&cache, &fp);

        let first = frame_bytes(10, 128);
        cache.cache_frame(source_id, 10, first.clone());
        cache.cache_frame(source_id, 10, vec![9; 128]);
        cache.sync();

        assert_eq!(cache.get(source_id, 10), Some(first));
        assert_eq!(cache.frame_count(source_id), 1);
        assert_eq!(cache.total_bytes(), 128);
    }

    #[test]
    fn cache_frame_to_unregistered_source_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        cache.cache_frame(0xbad, 1, frame_bytes(1, 64));
        cache.sync();
        assert_eq!(cache.total_bytes(), 0);
    }

    #[test]
    fn persists_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let fp = test_fingerprint("persist");
        let source_id;

        {
            let cache = open_cache(dir.path(), 1024 * 1024);
            source_id = register(&cache, &fp);
            cache_and_sync(&cache, source_id, 7, frame_bytes(7, 512));
        }

        let cache = open_cache(dir.path(), 1024 * 1024);
        let reopened = cache
            .register_source(fp, test_spec())
            .expect("re-register");
        assert_eq!(reopened, source_id);
        assert_eq!(cache.get(source_id, 7), Some(frame_bytes(7, 512)));
        assert_eq!(cache.frame_count(source_id), 1);
    }

    #[test]
    fn multiple_sources_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp_a = test_fingerprint("iso_a");
        let fp_b = test_fingerprint("iso_b");
        let id_a = register(&cache, &fp_a);
        let id_b = register(&cache, &fp_b);

        cache_and_sync(&cache, id_a, 1, frame_bytes(1, 64));
        cache_and_sync(&cache, id_b, 2, frame_bytes(2, 96));

        assert_eq!(cache.get(id_a, 1), Some(frame_bytes(1, 64)));
        assert!(cache.get(id_a, 2).is_none());
        assert_eq!(cache.get(id_b, 2), Some(frame_bytes(2, 96)));
        assert!(cache.get(id_b, 1).is_none());
        assert_eq!(cache.total_bytes(), 160);
    }

    #[test]
    fn spec_change_invalidates_index() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("spec");
        let source_id = register(&cache, &fp);
        cache_and_sync(&cache, source_id, 5, frame_bytes(5, 100));
        assert!(cache.contains(source_id, 5));

        let new_spec = CacheSpec {
            width: 1280,
            height: 720,
            pixfmt: "yuv420p".into(),
        };
        let same_id = cache
            .register_source(fp, new_spec)
            .expect("re-register new spec");
        assert_eq!(same_id, source_id);
        assert!(!cache.contains(source_id, 5));
        assert_eq!(cache.frame_count(source_id), 0);
        assert_eq!(cache.total_bytes(), 0);

        cache_and_sync(&cache, source_id, 5, frame_bytes(5, 80));
        assert_eq!(cache.get(source_id, 5), Some(frame_bytes(5, 80)));
    }

    #[test]
    fn spec_change_on_disk_reopen_clears_stale_index() {
        let dir = tempfile::tempdir().unwrap();
        let fp = test_fingerprint("disk_spec");
        let source_id;

        {
            let cache = open_cache(dir.path(), 1024 * 1024);
            source_id = register(&cache, &fp);
            cache_and_sync(&cache, source_id, 3, frame_bytes(3, 64));
        }

        let cache = open_cache(dir.path(), 1024 * 1024);
        let new_spec = CacheSpec {
            width: 3840,
            height: 2160,
            pixfmt: "yuv420p".into(),
        };
        cache
            .register_source(fp, new_spec)
            .expect("register new spec");
        assert!(!cache.contains(source_id, 3));
        assert!(cache.get(source_id, 3).is_none());
    }

    #[test]
    fn eviction_drops_oldest_frames_first() {
        let dir = tempfile::tempdir().unwrap();
        // Hold three 100-byte frames; adding a fourth should evict exactly one.
        let cache = open_cache(dir.path(), 350);
        let fp = test_fingerprint("evict");
        let source_id = register(&cache, &fp);

        for pts in [1_i64, 2, 3] {
            cache_and_sync(&cache, source_id, pts, frame_bytes(pts, 100));
        }
        assert_eq!(cache.total_bytes(), 300);

        cache_and_sync(&cache, source_id, 4, frame_bytes(4, 100));

        assert!(!cache.contains(source_id, 1));
        assert!(cache.contains(source_id, 2));
        assert!(cache.contains(source_id, 3));
        assert!(cache.contains(source_id, 4));
        assert_eq!(cache.total_bytes(), 300);
    }

    #[test]
    fn get_bumps_lru_and_protects_recent_frames() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 350);
        let fp = test_fingerprint("lru");
        let source_id = register(&cache, &fp);

        for pts in [1_i64, 2, 3] {
            cache_and_sync(&cache, source_id, pts, frame_bytes(pts, 100));
        }
        assert_eq!(cache.get(source_id, 2), Some(frame_bytes(2, 100)));

        cache_and_sync(&cache, source_id, 4, frame_bytes(4, 100));

        assert!(!cache.contains(source_id, 1));
        assert!(cache.contains(source_id, 2));
        assert!(cache.contains(source_id, 3));
        assert!(cache.contains(source_id, 4));
    }

    #[test]
    fn truncated_blob_resets_index() {
        let dir = tempfile::tempdir().unwrap();
        let fp = test_fingerprint("truncate");
        let source_id;
        let blob_path;

        {
            let cache = open_cache(dir.path(), 1024 * 1024);
            source_id = register(&cache, &fp);
            cache_and_sync(&cache, source_id, 9, frame_bytes(9, 128));
            blob_path = dir.path().join(format!("{source_id:016x}.yuv"));
        }

        std::fs::write(&blob_path, [0u8; 16]).unwrap();

        let cache = open_cache(dir.path(), 1024 * 1024);
        cache
            .register_source(fp, test_spec())
            .expect("re-register after truncate");
        assert!(!cache.contains(source_id, 9));
        assert!(cache.get(source_id, 9).is_none());
    }

    #[test]
    fn sync_waits_for_async_writes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("async");
        let source_id = register(&cache, &fp);

        cache.cache_frame(source_id, 50, frame_bytes(50, 32));
        cache.sync();
        assert!(cache.contains(source_id, 50));
        assert_eq!(cache.get(source_id, 50), Some(frame_bytes(50, 32)));
    }

    #[test]
    fn channel_backpressure_drops_frames_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let cache =
            FrameCache::with_channel_capacity(dir.path().to_path_buf(), 1024 * 1024, 2).unwrap();
        let fp = test_fingerprint("backpressure");
        let source_id = register(&cache, &fp);

        cache.cache_frame(source_id, 1, frame_bytes(1, 16));
        cache.cache_frame(source_id, 2, frame_bytes(2, 16));
        cache.cache_frame(source_id, 3, frame_bytes(3, 16));
        cache.sync();

        let cached: Vec<i64> = [1, 2, 3]
            .into_iter()
            .filter(|pts| cache.contains(source_id, *pts))
            .collect();
        assert!(
            cached.len() < 3,
            "expected at least one dropped frame, cached {cached:?}"
        );
        assert!(!cached.is_empty(), "expected some frames to land");
    }

    #[test]
    fn manifest_survives_crash_style_tmp_swap() {
        let dir = tempfile::tempdir().unwrap();
        let fp = test_fingerprint("manifest");
        let source_id;

        {
            let cache = open_cache(dir.path(), 1024 * 1024);
            source_id = register(&cache, &fp);
            cache_and_sync(&cache, source_id, 11, frame_bytes(11, 48));
            let idx = dir.path().join(format!("{source_id:016x}.idx"));
            assert!(idx.exists());
            assert!(!dir.path().join(format!("{source_id:016x}.idx.tmp")).exists());
        }

        let cache = open_cache(dir.path(), 1024 * 1024);
        cache
            .register_source(fp, test_spec())
            .expect("reopen persisted cache");
        assert_eq!(cache.get(source_id, 11), Some(frame_bytes(11, 48)));
    }

    #[test]
    fn drop_flushes_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let fp = test_fingerprint("drop_flush");
        let source_id;

        {
            let cache = open_cache(dir.path(), 1024 * 1024);
            source_id = register(&cache, &fp);
            cache_and_sync(&cache, source_id, 20, frame_bytes(20, 40));
        }

        let cache = open_cache(dir.path(), 1024 * 1024);
        cache
            .register_source(fp, test_spec())
            .expect("reopen after drop");
        assert_eq!(cache.get(source_id, 20), Some(frame_bytes(20, 40)));
    }

    #[test]
    fn partial_write_before_enospc_does_not_corrupt_next_frame() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("partial");
        let source_id = register(&cache, &fp);

        let frame_one = b"FRAMEONE12".to_vec();
        cache_and_sync(&cache, source_id, 1, frame_one.clone());

        let frame_two = b"REALFRAMEDATA".to_vec();
        cache.set_fail_writes_after_bytes_for_test(6);
        cache.cache_frame(source_id, 2, frame_two.clone());
        cache.sync();
        cache.set_fail_writes_after_bytes_for_test(0);
        assert!(cache.disk_pressure());
        assert!(!cache.contains(source_id, 2));

        cache.clear_disk_pressure();
        cache_and_sync(&cache, source_id, 2, frame_two.clone());

        assert_eq!(cache.get(source_id, 1), Some(frame_one));
        assert_eq!(cache.get(source_id, 2), Some(frame_two));
    }

    #[test]
    fn disk_full_sets_pressure_and_drops_write() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("disk_full");
        let source_id = register(&cache, &fp);

        cache_and_sync(&cache, source_id, 1, frame_bytes(1, 64));
        assert!(!cache.disk_pressure());

        cache.set_fail_writes_enospc_for_test(true);
        cache.cache_frame(source_id, 2, frame_bytes(2, 64));
        cache.sync();
        cache.set_fail_writes_enospc_for_test(false);

        assert!(cache.disk_pressure());
        assert!(cache.contains(source_id, 1));
        assert!(!cache.contains(source_id, 2));
    }

    #[test]
    fn disk_full_short_circuits_cache_frame() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("disk_short");
        let source_id = register(&cache, &fp);

        cache.set_fail_writes_enospc_for_test(true);
        cache.cache_frame(source_id, 1, frame_bytes(1, 64));
        cache.sync();
        cache.set_fail_writes_enospc_for_test(false);
        assert!(cache.disk_pressure());

        cache.cache_frame(source_id, 3, frame_bytes(3, 64));
        cache.sync();
        assert!(!cache.contains(source_id, 3));
    }

    #[test]
    fn successful_write_clears_disk_pressure() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("disk_recover");
        let source_id = register(&cache, &fp);

        cache.set_fail_writes_enospc_for_test(true);
        cache.cache_frame(source_id, 1, frame_bytes(1, 64));
        cache.sync();
        cache.set_fail_writes_enospc_for_test(false);
        assert!(cache.disk_pressure());

        cache.clear_disk_pressure();
        cache_and_sync(&cache, source_id, 2, frame_bytes(2, 64));

        assert!(!cache.disk_pressure());
        assert!(cache.contains(source_id, 2));
    }

    #[test]
    fn manifest_flush_disk_full_sets_pressure() {
        let dir = tempfile::tempdir().unwrap();
        let cache = open_cache(dir.path(), 1024 * 1024);
        let fp = test_fingerprint("flush_full");
        let source_id = register(&cache, &fp);

        cache_and_sync(&cache, source_id, 1, frame_bytes(1, 32));
        assert!(!cache.disk_pressure());

        cache.set_fail_flush_enospc_for_test(true);
        cache.sync();
        cache.set_fail_flush_enospc_for_test(false);
        assert!(cache.disk_pressure());

        // Manifest flush recovering does not imply blob writes work again.
        cache.sync();
        assert!(cache.disk_pressure());

        cache.clear_disk_pressure();
        cache_and_sync(&cache, source_id, 2, frame_bytes(2, 32));
        assert!(!cache.disk_pressure());
    }
}
