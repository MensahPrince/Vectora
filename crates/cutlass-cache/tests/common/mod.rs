//! Shared helpers for `cutlass-cache` integration tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cutlass_cache::{CacheSpec, FrameCache, SourceFingerprint, SourceId};

static SOURCE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn yuv420p_1080p_spec() -> CacheSpec {
    CacheSpec {
        width: 1920,
        height: 1080,
        pixfmt: "yuv420p".into(),
    }
}

pub fn frame_payload(pts: i64, len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    bytes[0] = (pts & 0xff) as u8;
    bytes[1] = ((pts >> 8) & 0xff) as u8;
    bytes[2] = ((pts >> 16) & 0xff) as u8;
    bytes
}

pub fn virtual_fingerprint(label: &str) -> SourceFingerprint {
    let n = SOURCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    SourceFingerprint {
        path: format!("/virtual/{label}_{n}"),
        size: 10_000 + n,
        mtime_ns: 1_700_000_000_000_000_000 + u128::from(n),
    }
}

pub fn open_cache(dir: &Path, budget_bytes: u64) -> FrameCache {
    FrameCache::new(dir.to_path_buf(), budget_bytes).expect("open frame cache")
}

pub fn register_source(cache: &FrameCache, fingerprint: &SourceFingerprint) -> SourceId {
    cache
        .register_source(fingerprint.clone(), yuv420p_1080p_spec())
        .expect("register source")
}

pub fn cache_frame_sync(cache: &FrameCache, source_id: SourceId, pts: i64, bytes: Vec<u8>) {
    cache.cache_frame(source_id, pts, bytes);
    cache.sync();
}

pub fn blob_path(cache_dir: &Path, source_id: SourceId) -> PathBuf {
    cache_dir.join(format!("{source_id:016x}.yuv"))
}

pub fn manifest_path(cache_dir: &Path, source_id: SourceId) -> PathBuf {
    cache_dir.join(format!("{source_id:016x}.idx"))
}
