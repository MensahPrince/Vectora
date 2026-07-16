use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError, Weak};
use std::time::Duration;

use super::catalog::{ModelIntegrityError, ModelManagerError, ModelSpec};

const MODEL_LOCK_RETRY_DELAY: Duration = Duration::from_millis(5);

pub(super) static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static MODEL_LOCKS: OnceLock<Mutex<ModelLockMap>> = OnceLock::new();

type ModelLockMap = HashMap<PathBuf, Weak<Mutex<()>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileVersion {
    length: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(windows)]
    creation_time: u64,
    #[cfg(windows)]
    last_write_time: u64,
    #[cfg(not(any(unix, windows)))]
    modified: Option<std::time::SystemTime>,
}

impl FileVersion {
    pub(super) fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            Self {
                length: metadata.len(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;

            Self {
                length: metadata.len(),
                creation_time: metadata.creation_time(),
                last_write_time: metadata.last_write_time(),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Self {
                length: metadata.len(),
                modified: metadata.modified().ok(),
            }
        }
    }
}

pub(super) fn validate_root(root: &Path) -> Result<PathBuf, ModelManagerError> {
    if !root.is_absolute() {
        return Err(ModelManagerError::RootNotAbsolute {
            root: root.to_path_buf(),
        });
    }

    let mut has_normal_component = false;
    for component in root.components() {
        match component {
            Component::ParentDir | Component::CurDir => {
                return Err(ModelManagerError::RootTraversal {
                    root: root.to_path_buf(),
                });
            }
            Component::Normal(_) => has_normal_component = true,
            Component::Prefix(_) | Component::RootDir => {}
        }
    }
    if !has_normal_component {
        return Err(ModelManagerError::FilesystemRoot {
            root: root.to_path_buf(),
        });
    }

    Ok(root.components().collect())
}

pub(super) fn validate_spec(spec: &ModelSpec) -> Result<(), ModelManagerError> {
    let filename = spec.filename();
    if filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename.contains('/')
        || filename.contains('\\')
    {
        return Err(ModelManagerError::InvalidCatalogEntry {
            model_id: spec.id(),
            detail: "filename must be one plain path component",
        });
    }
    let mut components = Path::new(filename).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(ModelManagerError::InvalidCatalogEntry {
            model_id: spec.id(),
            detail: "filename must be one plain path component",
        });
    }
    if spec.exact_bytes() == 0 {
        return Err(ModelManagerError::InvalidCatalogEntry {
            model_id: spec.id(),
            detail: "exact byte count must be non-zero",
        });
    }
    if spec.sha256().len() != 64
        || !spec
            .sha256()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ModelManagerError::InvalidCatalogEntry {
            model_id: spec.id(),
            detail: "SHA-256 must be 64 lowercase hexadecimal characters",
        });
    }
    Ok(())
}

pub(super) fn ensure_directory_root(root: &Path, metadata: &fs::Metadata) -> Result<(), ModelManagerError> {
    if metadata.file_type().is_symlink() || std_metadata_is_reparse_point(metadata) {
        return Err(ModelManagerError::UnsafeRoot {
            root: root.to_path_buf(),
            detail: "root is a symlink or reparse point",
        });
    }
    if !metadata.is_dir() {
        return Err(ModelManagerError::UnsafeRoot {
            root: root.to_path_buf(),
            detail: "root is not a directory",
        });
    }
    Ok(())
}

pub(super) fn ensure_regular_model_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ModelManagerError> {
    if metadata.file_type().is_symlink() || std_metadata_is_reparse_point(metadata) {
        return Err(ModelManagerError::UnsafeModelFile {
            path: path.to_path_buf(),
            detail: "model path is a symlink or reparse point",
        });
    }
    if !metadata.is_file() {
        return Err(ModelManagerError::UnsafeModelFile {
            path: path.to_path_buf(),
            detail: "model path is not a regular file",
        });
    }
    Ok(())
}

pub(super) fn ensure_cap_regular_model_file(
    path: &Path,
    metadata: &cap_std::fs::Metadata,
) -> Result<(), ModelManagerError> {
    if metadata.is_symlink() || cap_metadata_is_reparse_point(metadata) {
        return Err(ModelManagerError::UnsafeModelFile {
            path: path.to_path_buf(),
            detail: "model path is a symlink or reparse point",
        });
    }
    if !metadata.is_file() {
        return Err(ModelManagerError::UnsafeModelFile {
            path: path.to_path_buf(),
            detail: "model path is not a regular file",
        });
    }
    Ok(())
}

#[cfg(windows)]
pub(super) const WINDOWS_FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

#[cfg(windows)]
pub(super) fn windows_attributes_are_reparse_point(attributes: u32) -> bool {
    attributes & WINDOWS_FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(windows)]
pub(super) fn std_metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    windows_attributes_are_reparse_point(metadata.file_attributes())
}

#[cfg(not(windows))]
fn std_metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
pub(super) fn cap_metadata_is_reparse_point(metadata: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt as _;

    windows_attributes_are_reparse_point(metadata.file_attributes())
}

#[cfg(not(windows))]
fn cap_metadata_is_reparse_point(_metadata: &cap_std::fs::Metadata) -> bool {
    false
}

pub(super) fn integrity_error(path: PathBuf, reason: ModelIntegrityError) -> ModelManagerError {
    ModelManagerError::Integrity { path, reason }
}

pub(super) fn next_read_length(expected: u64, observed: u64, buffer_capacity: usize) -> usize {
    let remaining = expected.saturating_sub(observed);
    remaining.min((buffer_capacity - 1) as u64) as usize + 1
}

pub(super) fn digest_to_hex(digest: &[u8]) -> String {
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing hexadecimal to a string cannot fail");
    }
    output
}

pub(super) fn never_cancelled() -> bool {
    false
}

pub(super) fn check_cancelled(cancelled: &dyn Fn() -> bool) -> Result<(), ModelManagerError> {
    let requested =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(cancelled)).unwrap_or(true);
    if requested {
        Err(ModelManagerError::Cancelled)
    } else {
        Ok(())
    }
}

pub(super) fn check_begin_commit(begin_commit: &dyn Fn() -> bool) -> Result<(), ModelManagerError> {
    let accepted =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(begin_commit)).unwrap_or(false);
    if accepted {
        Ok(())
    } else {
        Err(ModelManagerError::Cancelled)
    }
}

pub(super) fn lock_model_with_cancellation<'a>(
    lock: &'a Mutex<()>,
    cancelled: &dyn Fn() -> bool,
) -> Result<MutexGuard<'a, ()>, ModelManagerError> {
    loop {
        check_cancelled(cancelled)?;
        match lock.try_lock() {
            Ok(guard) => {
                check_cancelled(cancelled)?;
                return Ok(guard);
            }
            Err(TryLockError::Poisoned(poisoned)) => {
                let guard = poisoned.into_inner();
                check_cancelled(cancelled)?;
                return Ok(guard);
            }
            Err(TryLockError::WouldBlock) => {
                std::thread::yield_now();
                std::thread::sleep(MODEL_LOCK_RETRY_DELAY);
            }
        }
    }
}

pub(super) fn model_lock(path: &Path) -> Arc<Mutex<()>> {
    let locks = MODEL_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }

    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

