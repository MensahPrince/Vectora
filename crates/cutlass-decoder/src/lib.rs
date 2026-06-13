//! Media demux + decode.
//!
//! Video decode lives under [`video`]; [`audio`] covers waveform peak
//! extraction and clocked playback streaming ([`AudioReader`]).

pub mod audio;
mod error;
pub mod image;
pub mod video;

pub use audio::{
    AUDIO_CHANNELS, AudioPeaks, AudioReader, audio_peaks, audio_peaks_per_second, render_stretched,
    render_stretched_curve,
};
pub use error::DecodeError;
pub use image::{STILL_MAX_DIM, decode_image};
pub use video::{
    DecodeOptions, DecodedFrame, Decoder, HwAccel, KeyframeIndex, PixelFormat, Plane, SourceInfo,
    ThumbnailImage, attach_hwaccel, duration_to_ticks, ffmpeg_version, hw_accel_from_env,
    is_hardware_pixel_format, ticks_to_duration, transfer_hw_frame_to_cpu, video_strip,
    video_thumbnail,
};

use tracing::info;

pub fn init() {
    info!(
        ffmpeg = %ffmpeg_version(),
        "cutlass-decoder ready"
    );
}
