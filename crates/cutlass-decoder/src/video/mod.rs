//! Video demux + decode.

mod decoder;
mod frame;
mod hwaccel;
mod keyframe_indexer;
mod scale;
mod strip;
mod thumbnail;

pub(crate) use decoder::ensure_ffmpeg_init;
pub use decoder::{Decoder, SourceInfo, ffmpeg_version, hw_accel_from_env};
pub use frame::{DecodedFrame, PixelFormat, Plane};
pub use hwaccel::{
    DecodeOptions, HwAccel, attach as attach_hwaccel, is_hardware_pixel_format,
    transfer_to_cpu as transfer_hw_frame_to_cpu,
};
pub use keyframe_indexer::{KeyframeIndex, duration_to_ticks, ticks_to_duration};
pub use scale::scale_yuv420p;
pub use strip::video_strip;
pub use thumbnail::{ThumbnailImage, video_thumbnail};
pub(crate) use thumbnail::{decode_first_frame, scale_to_rgba};
