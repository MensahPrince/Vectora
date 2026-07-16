//! Storage errors and operation reports.

use std::error::Error;
use std::fmt;
use std::io;

use crate::cache_registry::CacheId;

/// Maximum number of bytes retained from an underlying error message.
const MAX_ERROR_MESSAGE_BYTES: usize = 256;

/// Logical usage of a directory tree.
///
/// `bytes` sums `symlink_metadata().len()` for every non-directory entry.
/// `files` counts those entries, including symlinks, without following them.
/// Directory inode sizes are not included.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    /// Logical bytes across non-directory entries.
    pub bytes: u64,
    /// Number of non-directory entries.
    pub files: u64,
}

/// Result of clearing a cache directory.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClearReport {
    /// Logical bytes represented by removed non-directory entries.
    pub removed_bytes: u64,
    /// Number of removed non-directory entries.
    pub removed_files: u64,
}

/// Result of a successful cache relocation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelocationReport {
    /// Logical bytes present when relocation began.
    pub bytes: u64,
    /// Number of non-directory entries present when relocation began.
    pub files: u64,
    /// Whether rename crossed filesystems and the copy fallback was used.
    pub used_copy_fallback: bool,
}

/// Failures from cache layout and filesystem operations.
#[derive(Debug)]
pub enum StorageError {
    /// Cooperative cancellation was requested.
    Cancelled,
    /// A path that must be absolute was relative.
    PathNotAbsolute(&'static str),
    /// An empty path or filesystem root was supplied to a destructive API.
    DangerousPath(&'static str),
    /// An override key was not a registered cache.
    UnknownCacheId,
    /// An override was supplied for a memory cache.
    CacheIsNotDisk(CacheId),
    /// The same override key appeared more than once.
    DuplicateOverride(CacheId),
    /// Two cache roots overlap, so clearing either could remove the other.
    CachePathsOverlap {
        /// Cache path being validated or compared.
        cache: CacheId,
        /// Other cache path it overlaps.
        other: CacheId,
    },
    /// The operation root itself is a symbolic link.
    SymlinkRoot,
    /// A required operation root is not a directory.
    NotDirectory,
    /// The relocation source does not exist.
    SourceMissing,
    /// The relocation destination already exists.
    DestinationExists,
    /// Relocation source and destination are equal or nested.
    PathsOverlap,
    /// Copy fallback encountered a socket, device, FIFO, or other special file.
    UnsupportedFileType,
    /// Byte or file accounting exceeded `u64`.
    AccountingOverflow,
    /// Unique temporary sibling allocation exhausted its bounded attempts.
    TemporaryNameExhausted,
    /// An underlying filesystem operation failed.
    Io {
        /// Short static operation name.
        operation: &'static str,
        /// Operating-system error.
        source: io::Error,
    },
    /// Persistence rejected a complete destination and rollback succeeded.
    PersistenceFailed {
        /// Bounded callback error.
        message: String,
    },
    /// Persistence failed and the best-effort rollback also failed.
    RollbackFailed {
        /// Bounded callback error.
        persistence: String,
        /// Bounded rollback error.
        rollback: String,
    },
    /// Copy failed or was cancelled and its temporary tree could not be removed.
    TemporaryCleanupFailed {
        /// Bounded original error.
        original: String,
        /// Bounded cleanup error.
        cleanup: String,
    },
    /// Persistence committed the destination, but deleting the old copy failed.
    CommittedCleanupFailed {
        /// Bounded cleanup error.
        message: String,
        /// The relocation that was already committed and must be published.
        report: RelocationReport,
    },
}

impl StorageError {
    /// Return relocation accounting when the durable destination was committed
    /// even though best-effort cleanup of the old cross-device copy failed.
    ///
    /// Callers must publish the new root for this outcome. Treating it like a
    /// pre-commit failure would leave live state pointing at the stale source
    /// while persisted settings already name the destination.
    pub const fn committed_relocation(&self) -> Option<RelocationReport> {
        match self {
            Self::CommittedCleanupFailed { report, .. } => Some(*report),
            _ => None,
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("storage operation cancelled"),
            Self::PathNotAbsolute(role) => write!(f, "{role} must be absolute"),
            Self::DangerousPath(reason) => write!(f, "dangerous storage path: {reason}"),
            Self::UnknownCacheId => f.write_str("unknown cache override"),
            Self::CacheIsNotDisk(id) => write!(f, "cache {id} has no disk path"),
            Self::DuplicateOverride(id) => write!(f, "duplicate override for cache {id}"),
            Self::CachePathsOverlap { cache, other } => {
                write!(f, "cache paths for {cache} and {other} must not overlap")
            }
            Self::SymlinkRoot => f.write_str("operation root must not be a symbolic link"),
            Self::NotDirectory => f.write_str("operation root must be a directory"),
            Self::SourceMissing => f.write_str("relocation source does not exist"),
            Self::DestinationExists => f.write_str("relocation destination already exists"),
            Self::PathsOverlap => f.write_str("relocation source and destination must not overlap"),
            Self::UnsupportedFileType => {
                f.write_str("copy fallback encountered an unsupported file type")
            }
            Self::AccountingOverflow => f.write_str("storage usage accounting overflowed"),
            Self::TemporaryNameExhausted => {
                f.write_str("could not allocate a temporary relocation directory")
            }
            Self::Io { operation, source } => {
                write!(
                    f,
                    "{operation} failed: {}",
                    bounded_text(&source.to_string())
                )
            }
            Self::PersistenceFailed { message } => {
                write!(f, "persisting relocated cache failed: {message}")
            }
            Self::RollbackFailed {
                persistence,
                rollback,
            } => write!(
                f,
                "persisting relocated cache failed: {persistence}; rollback failed: {rollback}"
            ),
            Self::TemporaryCleanupFailed { original, cleanup } => write!(
                f,
                "{original}; temporary relocation cleanup failed: {cleanup}"
            ),
            Self::CommittedCleanupFailed { message, .. } => write!(
                f,
                "relocation committed, but old cache cleanup failed: {message}"
            ),
        }
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn bounded_text(message: &str) -> String {
    if message.len() <= MAX_ERROR_MESSAGE_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_ERROR_MESSAGE_BYTES.saturating_sub(3);
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut bounded = message[..end].to_owned();
    bounded.push_str("...");
    bounded
}
