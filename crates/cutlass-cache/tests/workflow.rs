//! End-to-end frame-cache workflows: register, async write, read, eviction, restart.

mod common;

use common::{
    blob_path, cache_frame_sync, frame_payload, manifest_path, open_cache, register_source,
    virtual_fingerprint, yuv420p_1080p_spec,
};
use cutlass_cache::SourceFingerprint;
use std::fs;

#[test]
fn scrub_session_caches_many_frames_and_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let cache = open_cache(dir.path(), 512 * 1024);
    let fp = virtual_fingerprint("scrub");
    let source_id = register_source(&cache, &fp);

    for pts in 0..60_i64 {
        cache_frame_sync(&cache, source_id, pts, frame_payload(pts, 4_096));
    }

    assert_eq!(cache.frame_count(source_id), 60);
    assert_eq!(cache.total_bytes(), 60 * 4_096);

    for pts in (0..60).step_by(5) {
        assert_eq!(cache.get(source_id, pts), Some(frame_payload(pts, 4_096)));
    }
    assert!(cache.get(source_id, 999).is_none());
}

#[test]
fn warm_restart_restores_indexed_frames() {
    let dir = tempfile::tempdir().unwrap();
    let fp = virtual_fingerprint("warm");
    let source_id;
    let pts_list: Vec<i64> = (0..12).collect();

    {
        let cache = open_cache(dir.path(), 1024 * 1024);
        source_id = register_source(&cache, &fp);
        for &pts in &pts_list {
            cache_frame_sync(&cache, source_id, pts, frame_payload(pts, 512));
        }
        assert!(manifest_path(dir.path(), source_id).exists());
        assert!(blob_path(dir.path(), source_id).exists());
    }

    let cache = open_cache(dir.path(), 1024 * 1024);
    let reopened = cache
        .register_source(fp, yuv420p_1080p_spec())
        .expect("re-register after restart");
    assert_eq!(reopened, source_id);
    assert_eq!(cache.frame_count(source_id), pts_list.len());

    for &pts in &pts_list {
        assert_eq!(cache.get(source_id, pts), Some(frame_payload(pts, 512)));
    }
}

#[test]
fn fingerprint_from_real_file_is_stable_until_metadata_changes() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("clip.mp4");
    fs::write(&media, b"initial-bytes").unwrap();

    let fp1 = SourceFingerprint::from_path(&media).unwrap();
    let id1 = fp1.id();

    let cache = open_cache(dir.path(), 1024 * 1024);
    let source_id = register_source(&cache, &fp1);
    cache_frame_sync(&cache, source_id, 0, frame_payload(0, 128));

    let fp2 = SourceFingerprint::from_path(&media).unwrap();
    assert_eq!(fp2.id(), id1);
    assert_eq!(
        cache
            .register_source(fp2, yuv420p_1080p_spec())
            .expect("re-register same file"),
        source_id
    );
    assert!(cache.contains(source_id, 0));

    fs::write(&media, b"much-longer-file-contents").unwrap();
    let fp3 = SourceFingerprint::from_path(&media).unwrap();
    assert_ne!(fp3.id(), id1);

    let new_id = cache
        .register_source(fp3.clone(), yuv420p_1080p_spec())
        .expect("register after file growth");
    assert_eq!(new_id, fp3.id());
    // Size/mtime change yields a new source id; prior frames stay under the old id.
    assert!(cache.contains(source_id, 0));
    assert!(!cache.contains(new_id, 0));

    cache_frame_sync(&cache, new_id, 0, frame_payload(0, 256));
    assert_eq!(cache.get(new_id, 0), Some(frame_payload(0, 256)));
}

#[test]
fn cross_source_eviction_is_global_lru() {
    let dir = tempfile::tempdir().unwrap();
    // Room for four 100-byte frames before the fifth write triggers eviction.
    let cache = open_cache(dir.path(), 450);
    let fp_a = virtual_fingerprint("src_a");
    let fp_b = virtual_fingerprint("src_b");
    let a = register_source(&cache, &fp_a);
    let b = register_source(&cache, &fp_b);

    for pts in [1_i64, 2, 3] {
        cache_frame_sync(&cache, a, pts, frame_payload(pts, 100));
    }
    cache_frame_sync(&cache, b, 1, frame_payload(101, 100));
    assert_eq!(cache.total_bytes(), 400);

    // Touch source A frame 2 so frame 1 is coldest globally.
    assert_eq!(cache.get(a, 2), Some(frame_payload(2, 100)));

    cache_frame_sync(&cache, b, 2, frame_payload(102, 100));

    assert!(
        !cache.contains(a, 1),
        "oldest untouched frame should be evicted"
    );
    assert!(cache.contains(a, 2));
    assert!(cache.contains(a, 3));
    assert!(cache.contains(b, 1));
    assert!(cache.contains(b, 2));
    assert!(cache.total_bytes() <= 450);
}

#[test]
fn two_source_timeline_simulation() {
    let dir = tempfile::tempdir().unwrap();
    let cache = open_cache(dir.path(), 256 * 1024);
    let interview = virtual_fingerprint("interview");
    let broll = virtual_fingerprint("broll");
    let interview_id = register_source(&cache, &interview);
    let broll_id = register_source(&cache, &broll);

    for pts in 0..30 {
        cache_frame_sync(&cache, interview_id, pts, frame_payload(pts, 2_048));
    }
    for pts in 0..20 {
        cache_frame_sync(&cache, broll_id, pts * 2, frame_payload(pts * 2, 1_024));
    }

    assert_eq!(cache.frame_count(interview_id), 30);
    assert_eq!(cache.frame_count(broll_id), 20);
    assert_eq!(cache.total_bytes(), 30 * 2_048 + 20 * 1_024);

    assert_eq!(cache.get(broll_id, 10), Some(frame_payload(10, 1_024)));
    assert!(cache.get(interview_id, 29).is_some());
    assert!(cache.get(broll_id, 99).is_none());
}

#[test]
fn disk_pressure_lifecycle_across_session() {
    let dir = tempfile::tempdir().unwrap();
    let cache = open_cache(dir.path(), 1024 * 1024);
    let fp = virtual_fingerprint("pressure");
    let source_id = register_source(&cache, &fp);

    cache_frame_sync(&cache, source_id, 1, frame_payload(1, 64));
    assert!(!cache.disk_pressure());

    cache.set_fail_writes_enospc_for_test(true);
    cache.cache_frame(source_id, 2, frame_payload(2, 64));
    cache.sync();
    cache.set_fail_writes_enospc_for_test(false);

    assert!(cache.disk_pressure());
    assert!(cache.contains(source_id, 1));
    assert!(!cache.contains(source_id, 2));

    cache.cache_frame(source_id, 3, frame_payload(3, 64));
    cache.sync();
    assert!(!cache.contains(source_id, 3));

    cache.clear_disk_pressure();
    cache_frame_sync(&cache, source_id, 4, frame_payload(4, 64));
    assert!(!cache.disk_pressure());
    assert_eq!(cache.get(source_id, 4), Some(frame_payload(4, 64)));
}

#[test]
fn cloned_cache_handle_shares_state() {
    let dir = tempfile::tempdir().unwrap();
    let cache = open_cache(dir.path(), 1024 * 1024);
    let clone = cache.clone();
    let fp = virtual_fingerprint("shared");
    let source_id = register_source(&cache, &fp);

    cache_frame_sync(&cache, source_id, 7, frame_payload(7, 256));
    assert_eq!(clone.get(source_id, 7), Some(frame_payload(7, 256)));
    assert_eq!(clone.frame_count(source_id), 1);
}

#[test]
fn pixfmt_change_invalidates_prior_frames_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let fp = virtual_fingerprint("pixfmt");
    let source_id;

    {
        let cache = open_cache(dir.path(), 1024 * 1024);
        source_id = register_source(&cache, &fp);
        cache_frame_sync(&cache, source_id, 0, frame_payload(0, 200));
        assert!(cache.contains(source_id, 0));
    }

    let cache = open_cache(dir.path(), 1024 * 1024);
    let packed = cutlass_cache::CacheSpec {
        width: 1920,
        height: 1080,
        pixfmt: "yuv420p_packed".into(),
    };
    cache
        .register_source(fp, packed)
        .expect("register new pixfmt");
    assert!(!cache.contains(source_id, 0));

    cache_frame_sync(&cache, source_id, 0, frame_payload(0, 180));
    assert_eq!(cache.get(source_id, 0), Some(frame_payload(0, 180)));
}
