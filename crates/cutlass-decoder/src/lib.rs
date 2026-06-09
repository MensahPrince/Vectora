//! Media demux + decode.
//!
//! Video decode lives under [`video`]; audio will follow in a later module.

mod error;
pub mod video;

pub use error::DecodeError;
pub use video::{
    DecodeOptions, DecodedFrame, Decoder, HwAccel, KeyframeIndex, PixelFormat, Plane, SourceInfo,
    attach_hwaccel, duration_to_ticks, ffmpeg_version, hw_accel_from_env, is_hardware_pixel_format,
    ticks_to_duration, transfer_hw_frame_to_cpu,
};

use tracing::info;

pub fn init() {
    info!(
        ffmpeg = %ffmpeg_version(),
        "cutlass-decoder ready"
    );
}
