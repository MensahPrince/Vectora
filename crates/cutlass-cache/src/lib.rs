//! Frame and proxy caching for Cutlass.
//!
//! On-disk storage lives under [`diskcache`]; in-memory eviction and lookup will
//! be added here as the engine is wired back up.

pub mod diskcache;

pub use diskcache::{CacheSpec, DiskCacheError, FrameCache, SourceFingerprint, SourceId};

use tracing::info;

pub fn init() {
    info!("cutlass-cache ready");
}
