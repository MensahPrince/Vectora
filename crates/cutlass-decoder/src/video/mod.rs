//! Video demux + decode.

mod decoder;
mod frame;
mod hwaccel;
mod keyframe_indexer;

pub use decoder::{Decoder, SourceInfo, ffmpeg_version, hw_accel_from_env};
pub use frame::{DecodedFrame, PixelFormat, Plane};
pub use hwaccel::{
    DecodeOptions, HwAccel, attach as attach_hwaccel, is_hardware_pixel_format,
    transfer_to_cpu as transfer_hw_frame_to_cpu,
};
pub use keyframe_indexer::{KeyframeIndex, duration_to_ticks, ticks_to_duration};
