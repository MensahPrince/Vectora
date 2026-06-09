//! Session-scoped editing runtime.

use std::path::PathBuf;

use cutlass_cache::diskcache::FrameCache;

/// Default on-disk frame cache budget (50 GiB).
pub const DEFAULT_CACHE_BUDGET_BYTES: u64 = 50 * 1024 * 1024 * 1024;

/// Session configuration for [`Engine`].
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Directory for per-source YUV frame blobs and index sidecars.
    pub cache_dir: PathBuf,
    /// Global frame cache byte budget; LRU eviction runs across all sources.
    pub cache_budget_bytes: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from(".cutlass/cache"),
            cache_budget_bytes: DEFAULT_CACHE_BUDGET_BYTES,
        }
    }
}

/// Cutlass editing engine.
///
/// Owns session infrastructure (frame cache today; timeline state and source
/// readers will live here as the engine grows).
pub struct Engine {
    cache: FrameCache,
    config: EngineConfig,
}

impl Engine {
    pub fn new(config: EngineConfig) -> std::io::Result<Self> {
        let cache = FrameCache::new(config.cache_dir.clone(), config.cache_budget_bytes)?;
        Ok(Self { cache, config })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn cache(&self) -> &FrameCache {
        &self.cache
    }

    /// True when the frame cache has paused writes due to a disk-full I/O error.
    pub fn disk_pressure(&self) -> bool {
        self.cache.disk_pressure()
    }

    /// Resume cache writes after disk space is available again.
    pub fn clear_disk_pressure(&self) {
        self.cache.clear_disk_pressure();
    }
}
