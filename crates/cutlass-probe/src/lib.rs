//! Media probing: inspect files for container, codec, and stream metadata
//! without opening a full decode pipeline.
//!
//! Intended as the ffprobe-style layer used at import time (duration, frame
//! rate, resolution, audio layout) before `cutlass-decoder` takes over.

mod error;

pub use error::ProbeError;

use tracing::info;

pub fn init() {
    info!("cutlass-probe ready");
}
