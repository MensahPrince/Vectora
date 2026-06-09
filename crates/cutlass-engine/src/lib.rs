//! Editing engine.

mod engine;

pub use engine::{DEFAULT_CACHE_BUDGET_BYTES, Engine, EngineConfig};

use tracing::info;

pub fn init() {
    info!("cutlass-engine ready");
}
