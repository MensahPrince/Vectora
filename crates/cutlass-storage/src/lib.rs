//! Stable cache metadata and safety-focused filesystem operations.
//!
//! This crate deliberately stays `std`-only. It owns the identifiers that
//! cross settings, UI, and provider boundaries, plus the low-level operations
//! those layers use to inspect, clear, and relocate cache directories.
//!
//! Directory walks are iterative and inspect entries with
//! [`std::fs::symlink_metadata`], so directory symlinks are never traversed.
//! Callers should avoid concurrently mutating a tree while an operation is in
//! progress; the standard library does not expose race-free `openat`-style
//! traversal on every supported platform.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod cache_registry;
mod error;
mod layout;
mod ops;

#[cfg(test)]
mod tests;

pub use cache_registry::{
    CACHE_REGISTRY, CacheDescriptor, CacheId, CacheKind, CacheTier, ParseCacheIdError,
    cache_descriptor, cache_descriptor_by_key, cache_descriptors,
};
pub use error::{ClearReport, DiskUsage, RelocationReport, StorageError};
pub use layout::{
    SharedStorageLayout, SharedStorageLayoutError, SharedStorageLayoutTransitionError,
    StorageLayout, StorageLayoutLease, StorageLayoutSnapshot,
};
pub use ops::{Cancellation, NeverCancelled, clear_cache, measure_disk_usage, relocate_cache};
