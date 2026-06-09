//! On-disk cache layout and I/O.
//!
//! Planned: proxy transcodes, per-frame blobs, mmap-friendly flat stores, and
//! cache metadata (see `frames/cache_*` sample layouts in the repo).

mod cache;
mod error;

pub use cache::{
    CacheSpec, FrameCache, FrameEntry, SourceFingerprint, SourceId, SourceManifest, WriteMsg,
};
pub use error::DiskCacheError;
