//! Timeline exporter sink: encode a stream of composited **RGBA8** frames into
//! an H.264 MP4.
//!
//! This is the encode half of "composite every frame `0..duration` -> mux to
//! mp4". It is deliberately source-agnostic: it knows nothing about the
//! timeline, only how to take fully-rendered RGBA canvases (one per output
//! frame, in order) and turn them into a constant-frame-rate file. The engine
//! owns the loop that produces those canvases, so this same sink backs both the
//! Rust export path and the future Python `export`.
//!
//! Unlike [`build_proxy`](crate::proxy::build_proxy) — which is all-intra
//! (GOP-1) for fast seeking — the export uses the encoder's default GOP so the
//! deliverable is bit-efficient rather than scrub-optimized.

use std::path::Path;

use ffmpeg_next::format::{self, context::Output};
use ffmpeg_next::software::scaling;
use ffmpeg_next::util::format::Pixel;
use ffmpeg_next::util::frame::video::Video as VideoFrame;
use ffmpeg_next::{Dictionary, Rational, codec, encoder::Video as VideoEncoder};
use tracing::debug;

use crate::error::EncodeError;
use crate::h264::{
    drain_encoder, ensure_ffmpeg_init, fill_rgba_frame, fill_yuv420p_frame, find_h264_encoder,
};

/// How to encode an exported timeline.
#[derive(Debug, Clone, Copy)]
pub struct ExportConfig {
    /// Output width in pixels. Must be non-zero and even (H.264 4:2:0).
    pub width: u32,
    /// Output height in pixels. Must be non-zero and even (H.264 4:2:0).
    pub height: u32,
    /// Output frame rate numerator (frames per `frame_rate_den` seconds).
    pub frame_rate_num: i32,
    /// Output frame rate denominator.
    pub frame_rate_den: i32,
    /// Constant-quality level (libx264 CRF, 0–51, lower = better). Used on the
    /// software path; ~18 is visually near-transparent.
    pub quality: u8,
    /// Target bitrate (bits/sec) for hardware encoders without a CRF mode.
    pub bitrate: usize,
    /// Prefer a hardware H.264 encoder. Off by default for export: software
    /// libx264 at constant quality gives the cleanest deliverable.
    pub hardware: bool,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frame_rate_num: 30,
            frame_rate_den: 1,
            quality: 18,
            bitrate: 12_000_000,
            hardware: false,
        }
    }
}

/// Result of a completed export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportStats {
    pub frames: u64,
    pub width: u32,
    pub height: u32,
}

/// A push-based H.264/MP4 encoder fed one RGBA8 canvas per output frame.
///
/// Create it with [`VideoExport::create`], call [`push_rgba`](Self::push_rgba)
/// once per timeline frame in order, then [`finish`](Self::finish) to flush the
/// encoder and write the container trailer. Frames are presented on a clean
/// `1/fps` timeline (pts = frame index), so the file is constant-frame-rate.
pub struct VideoExport {
    octx: Output,
    encoder: VideoEncoder,
    scaler: scaling::Context,
    rgba: VideoFrame,
    ost_index: usize,
    enc_tb: Rational,
    ost_tb: Rational,
    frame_index: i64,
    width: u32,
    height: u32,
    finished: bool,
}

impl VideoExport {
    /// Open `output` and prepare the encoder for `config.width`×`config.height`
    /// RGBA frames at the given frame rate. Writes the container header.
    pub fn create(output: &Path, config: ExportConfig) -> Result<Self, EncodeError> {
        ensure_ffmpeg_init()?;

        let (width, height) = (config.width, config.height);
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err(EncodeError::unsupported(
                "export dimensions must be non-zero and even",
            ));
        }
        if config.frame_rate_num <= 0 || config.frame_rate_den <= 0 {
            return Err(EncodeError::unsupported(
                "export frame rate must be positive",
            ));
        }

        let fps = Rational::new(config.frame_rate_num, config.frame_rate_den);
        let enc_tb = fps.invert();

        let codec = find_h264_encoder(config.hardware)
            .ok_or_else(|| EncodeError::unsupported("no H.264 encoder available"))?;
        let use_crf = codec.name() == "libx264";

        let mut octx = format::output(output).map_err(EncodeError::Open)?;
        let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

        let mut enc = codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(EncodeError::Open)?;
        enc.set_width(width);
        enc.set_height(height);
        enc.set_format(Pixel::YUV420P);
        enc.set_color_range(ffmpeg_next::color::Range::MPEG);
        enc.set_frame_rate(Some(fps));
        enc.set_time_base(enc_tb);
        if !use_crf {
            enc.set_bit_rate(config.bitrate);
        }
        if global_header {
            enc.set_flags(codec::Flags::GLOBAL_HEADER);
        }

        let mut enc_opts = Dictionary::new();
        if use_crf {
            let crf = config.quality.to_string();
            enc_opts.set("crf", &crf);
            enc_opts.set("preset", "medium");
        }
        let encoder = enc.open_with(enc_opts).map_err(EncodeError::Open)?;

        let ost_index = {
            let mut ost = octx.add_stream(codec).map_err(EncodeError::Open)?;
            ost.set_parameters(&encoder);
            ost.index()
        };

        octx.write_header().map_err(EncodeError::Io)?;
        let ost_tb = octx.stream(ost_index).unwrap().time_base();

        let scaler = scaling::Context::get(
            Pixel::RGBA,
            width,
            height,
            Pixel::YUV420P,
            width,
            height,
            scaling::Flags::BILINEAR,
        )
        .map_err(EncodeError::Encode)?;

        let rgba = VideoFrame::new(Pixel::RGBA, width, height);

        Ok(Self {
            octx,
            encoder,
            scaler,
            rgba,
            ost_index,
            enc_tb,
            ost_tb,
            frame_index: 0,
            width,
            height,
            finished: false,
        })
    }

    /// Encode one frame from a row-major RGBA8 buffer (`width*height*4` bytes).
    ///
    /// Frames are consumed in presentation order; the Nth call becomes output
    /// frame N. The buffer is copied into the encoder's pipeline, so the caller
    /// may reuse or drop it immediately after this returns.
    /// Encode one frame from tight YUV420P planes (typical GPU readback layout).
    pub fn push_yuv420p(
        &mut self,
        width: u32,
        height: u32,
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Result<(), EncodeError> {
        if width != self.width || height != self.height {
            return Err(EncodeError::unsupported(format!(
                "yuv frame is {width}x{height}, expected {}x{}",
                self.width, self.height
            )));
        }
        let mut yuv = VideoFrame::new(Pixel::YUV420P, width, height);
        fill_yuv420p_frame(&mut yuv, width, height, y, u, v)?;
        yuv.set_pts(Some(self.frame_index));
        self.encoder.send_frame(&yuv).map_err(EncodeError::Encode)?;
        self.frame_index += 1;

        drain_encoder(
            &mut self.encoder,
            &mut self.octx,
            self.ost_index,
            self.enc_tb,
            self.ost_tb,
        )
    }

    /// Legacy: encode from RGBA8 via CPU FFmpeg scaler.
    pub fn push_rgba(&mut self, rgba: &[u8]) -> Result<(), EncodeError> {
        fill_rgba_frame(&mut self.rgba, self.width, self.height, rgba)?;

        let mut yuv = VideoFrame::empty();
        self.scaler
            .run(&self.rgba, &mut yuv)
            .map_err(EncodeError::Encode)?;
        yuv.set_pts(Some(self.frame_index));
        self.encoder.send_frame(&yuv).map_err(EncodeError::Encode)?;
        self.frame_index += 1;

        drain_encoder(
            &mut self.encoder,
            &mut self.octx,
            self.ost_index,
            self.enc_tb,
            self.ost_tb,
        )
    }

    /// Flush the encoder, write the container trailer, and return frame stats.
    pub fn finish(mut self) -> Result<ExportStats, EncodeError> {
        self.finish_inner()?;
        Ok(ExportStats {
            frames: self.frame_index as u64,
            width: self.width,
            height: self.height,
        })
    }

    fn finish_inner(&mut self) -> Result<(), EncodeError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.encoder.send_eof().map_err(EncodeError::Encode)?;
        drain_encoder(
            &mut self.encoder,
            &mut self.octx,
            self.ost_index,
            self.enc_tb,
            self.ost_tb,
        )?;
        self.octx.write_trailer().map_err(EncodeError::Io)?;
        debug!(
            frames = self.frame_index,
            self.width, self.height, "exported timeline"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cutlass_decoder::{DecodeOptions, Decoder, HwAccel};

    use super::*;

    #[test]
    fn exports_rgba_frames_to_seekable_mp4() {
        let out = std::env::temp_dir().join("cutlass_export_smoke.mp4");
        let _ = std::fs::remove_file(&out);

        let (w, h) = (64u32, 48u32);
        let mut sink = VideoExport::create(
            &out,
            ExportConfig {
                width: w,
                height: h,
                frame_rate_num: 30,
                frame_rate_den: 1,
                quality: 23,
                bitrate: 2_000_000,
                hardware: false,
            },
        )
        .expect("create export");

        let frame: Vec<u8> = std::iter::repeat_n([220u8, 30, 30, 255], (w * h) as usize)
            .flatten()
            .collect();
        for _ in 0..15 {
            sink.push_rgba(&frame).expect("push frame");
        }
        let stats = sink.finish().expect("finish export");

        assert_eq!(stats.frames, 15);
        assert_eq!((stats.width, stats.height), (w, h));
        assert!(out.exists(), "export file missing");

        let mut dec = Decoder::open_with(&out, DecodeOptions::default().hw_accel(HwAccel::None))
            .expect("open export");
        let decoded = dec
            .seek_to_frame(Duration::from_millis(200))
            .expect("seek export")
            .expect("frame after seek");
        assert!(decoded.width > 0 && !decoded.planes.is_empty());

        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn rejects_odd_or_zero_dimensions() {
        let out = std::env::temp_dir().join("cutlass_export_baddims.mp4");
        let err = VideoExport::create(
            &out,
            ExportConfig {
                width: 65,
                height: 48,
                ..Default::default()
            },
        );
        assert!(err.is_err(), "odd width should be rejected");
    }

    #[test]
    fn rejects_wrong_buffer_size() {
        let out = std::env::temp_dir().join("cutlass_export_badbuf.mp4");
        let _ = std::fs::remove_file(&out);
        let mut sink = VideoExport::create(
            &out,
            ExportConfig {
                width: 8,
                height: 8,
                hardware: false,
                ..Default::default()
            },
        )
        .expect("create export");
        assert!(sink.push_rgba(&[0u8; 10]).is_err());
        let _ = std::fs::remove_file(&out);
    }
}
