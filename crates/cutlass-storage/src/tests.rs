use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::ops::{RelocationStrategy, relocate_with_strategy};

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        for _ in 0..128 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("cutlass-storage-unit-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory: {error}"),
            }
        }
        panic!("could not allocate test directory");
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn shared_layout_generation_exhaustion_fails_closed() {
    let temporary = TestDirectory::new();
    let original = StorageLayout::new(temporary.path.join("original")).unwrap();
    let replacement = StorageLayout::new(temporary.path.join("replacement")).unwrap();
    let shared = SharedStorageLayout::with_generation_for_test(original.clone(), u64::MAX);

    assert_eq!(
        shared.replace(u64::MAX, replacement),
        Err(SharedStorageLayoutError::GenerationExhausted)
    );

    let update_was_called = std::cell::Cell::new(false);
    assert_eq!(
        shared.update(u64::MAX, |_| update_was_called.set(true)),
        Err(SharedStorageLayoutError::GenerationExhausted)
    );
    assert!(!update_was_called.get());

    let snapshot = shared.snapshot();
    assert_eq!(snapshot.generation(), u64::MAX);
    assert_eq!(snapshot.layout(), &original);
}

#[test]
fn forced_copy_strategy_exercises_fallback_without_second_volume() {
    let temporary = TestDirectory::new();
    let old_path = temporary.path.join("old");
    let new_path = temporary.path.join("new");
    fs::create_dir(&old_path).unwrap();
    fs::create_dir(old_path.join("nested")).unwrap();
    fs::write(old_path.join("nested").join("data"), b"fallback").unwrap();

    let report = relocate_with_strategy(
        &old_path,
        &new_path,
        &NeverCancelled,
        |completed| {
            assert_eq!(
                fs::read(completed.join("nested").join("data")).unwrap(),
                b"fallback"
            );
            assert!(old_path.exists(), "copy source must survive until persist");
            Ok(())
        },
        RelocationStrategy::ForceCopy,
    )
    .unwrap();

    assert!(report.used_copy_fallback);
    assert_eq!(report.bytes, 8);
    assert_eq!(report.files, 1);
    assert!(!old_path.exists());
    assert_eq!(
        fs::read(new_path.join("nested").join("data")).unwrap(),
        b"fallback"
    );
}

#[test]
fn forced_copy_persistence_failure_removes_destination_not_source() {
    let temporary = TestDirectory::new();
    let old_path = temporary.path.join("old");
    let new_path = temporary.path.join("new");
    fs::create_dir(&old_path).unwrap();
    fs::write(old_path.join("data"), b"authoritative").unwrap();

    let error = relocate_with_strategy(
        &old_path,
        &new_path,
        &NeverCancelled,
        |_| Err("settings unavailable".into()),
        RelocationStrategy::ForceCopy,
    )
    .unwrap_err();

    assert!(matches!(error, StorageError::PersistenceFailed { .. }));
    assert_eq!(fs::read(old_path.join("data")).unwrap(), b"authoritative");
    assert!(!new_path.exists());
}

#[test]
fn persistence_errors_are_bounded() {
    let temporary = TestDirectory::new();
    let old_path = temporary.path.join("old");
    let new_path = temporary.path.join("new");
    fs::create_dir(&old_path).unwrap();

    let error = relocate_cache(&old_path, &new_path, &NeverCancelled, |_| {
        Err("x".repeat(16_384))
    })
    .unwrap_err();
    assert!(error.to_string().len() < 400);
    assert!(old_path.exists());
    assert!(!new_path.exists());
}

#[test]
fn rollback_failure_is_reported_explicitly() {
    let temporary = TestDirectory::new();
    let old_path = temporary.path.join("old");
    let new_path = temporary.path.join("new");
    fs::create_dir(&old_path).unwrap();
    fs::write(old_path.join("original"), b"data").unwrap();

    let error = relocate_cache(&old_path, &new_path, &NeverCancelled, |_| {
        fs::create_dir(&old_path).unwrap();
        Err("settings unavailable".into())
    })
    .unwrap_err();

    assert!(matches!(error, StorageError::RollbackFailed { .. }));
    assert_eq!(fs::read(new_path.join("original")).unwrap(), b"data");
}
