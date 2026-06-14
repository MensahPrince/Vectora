//! Persistence and on-disk layout integration tests.

mod common;

use common::{
    blob_path, cache_frame_sync, frame_payload, manifest_path, open_cache, register_source,
    virtual_fingerprint, yuv420p_1080p_spec,
};
use cutlass_cache::FrameCache;
use std::fs;

#[test]
fn manifest_and_blob_files_use_source_id_hex_names() {
    let dir = tempfile::tempdir().unwrap();
    let fp = virtual_fingerprint("layout");
    let cache = open_cache(dir.path(), 1024 * 1024);
    let source_id = register_source(&cache, &fp);

    cache_frame_sync(&cache, source_id, 42, frame_payload(42, 64));

    let blob = blob_path(dir.path(), source_id);
    let idx = manifest_path(dir.path(), source_id);
    assert!(blob.exists());
    assert!(idx.exists());
    assert!(blob.metadata().unwrap().len() >= 64);
    assert!(idx.metadata().unwrap().len() > 0);
    assert!(
        !dir.path()
            .join(format!("{source_id:016x}.idx.tmp"))
            .exists()
    );
}

#[test]
fn drop_without_explicit_sync_still_persists_via_writer_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let fp = virtual_fingerprint("drop");
    let source_id;

    {
        let cache = open_cache(dir.path(), 1024 * 1024);
        source_id = register_source(&cache, &fp);
        cache.cache_frame(source_id, 5, frame_payload(5, 48));
        cache.sync();
        cache.cache_frame(source_id, 6, frame_payload(6, 48));
        // Drop runs writer thread join + final flush.
    }

    let cache = open_cache(dir.path(), 1024 * 1024);
    cache
        .register_source(fp, yuv420p_1080p_spec())
        .expect("reopen");
    assert!(cache.contains(source_id, 5));
    assert_eq!(cache.get(source_id, 5), Some(frame_payload(5, 48)));
    assert_eq!(cache.get(source_id, 6), Some(frame_payload(6, 48)));
}

#[test]
fn truncated_blob_on_disk_clears_stale_index_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let fp = virtual_fingerprint("truncate");
    let source_id;

    {
        let cache = open_cache(dir.path(), 1024 * 1024);
        source_id = register_source(&cache, &fp);
        cache_frame_sync(&cache, source_id, 9, frame_payload(9, 128));
    }

    let blob = blob_path(dir.path(), source_id);
    fs::write(&blob, [0u8; 32]).unwrap();

    let cache = open_cache(dir.path(), 1024 * 1024);
    cache
        .register_source(fp, yuv420p_1080p_spec())
        .expect("reopen after truncate");
    assert!(!cache.contains(source_id, 9));
    assert!(cache.get(source_id, 9).is_none());
    assert_eq!(cache.frame_count(source_id), 0);
}

#[test]
fn re_register_same_source_after_restart_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let fp = virtual_fingerprint("idempotent");
    let source_id;

    {
        let cache = open_cache(dir.path(), 1024 * 1024);
        source_id = register_source(&cache, &fp);
        cache_frame_sync(&cache, source_id, 1, frame_payload(1, 100));
    }

    let cache = open_cache(dir.path(), 1024 * 1024);
    let first = cache
        .register_source(fp.clone(), yuv420p_1080p_spec())
        .unwrap();
    let second = cache.register_source(fp, yuv420p_1080p_spec()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first, source_id);
    assert_eq!(cache.frame_count(source_id), 1);
}

#[test]
fn new_cache_dir_is_created_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("deep").join("cache");
    assert!(!nested.exists());

    let _cache = FrameCache::new(nested.clone(), 1024).expect("create nested cache dir");
    assert!(nested.is_dir());
}
