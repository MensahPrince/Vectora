use std::sync::OnceLock;

use ffmpeg_next::error::EAGAIN;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::format::context::Output;
use ffmpeg_next::util::frame::video::Video as VideoFrame;
use ffmpeg_next::{Codec, Error as FfmpegError, Packet, Rational, codec, encoder};

use crate::error::EncodeError;

static FFMPEG_INIT: OnceLock<Result<(), FfmpegError>> = OnceLock::new();

pub(crate) fn ensure_ffmpeg_init() -> Result<(), EncodeError> {
    match FFMPEG_INIT.get_or_init(ffmpeg_next::init) {
        Ok(()) => Ok(()),
        Err(e) => Err(EncodeError::Open(*e)),
    }
}

/// Write out every packet the encoder can currently produce. Works for any
/// stream kind — video and audio encoders both deref to the raw encoder.
pub(crate) fn drain_encoder(
    encoder: &mut encoder::Encoder,
    octx: &mut Output,
    ost_index: usize,
    enc_tb: Rational,
    ost_tb: Rational,
) -> Result<(), EncodeError> {
    let mut packet = Packet::empty();
    loop {
        match encoder.receive_packet(&mut packet) {
            Ok(()) => {
                packet.set_stream(ost_index);
                packet.rescale_ts(enc_tb, ost_tb);
                packet.write_interleaved(octx).map_err(EncodeError::Io)?;
            }
            Err(FfmpegError::Eof) => return Ok(()),
            Err(e) if is_eagain(&e) => return Ok(()),
            Err(e) => return Err(EncodeError::Encode(e)),
        }
    }
}

/// A chosen H.264 encoder together with the **software** pixel format its frames
/// must be fed in. Always a CPU layout we can produce with swscale — never a
/// hardware-surface format — so the caller can blindly upload composited frames.
#[derive(Clone, Copy)]
pub(crate) struct H264Encoder {
    pub codec: Codec,
    pub pixel: Pixel,
}

/// Software input layouts we know how to fill/convert to, most-preferred first.
/// `YUV420P` is the planar layout the compositor reads back; `NV12` is what
/// several OS encoders (notably Windows Media Foundation) want instead.
const SOFTWARE_INPUT_FORMATS: [Pixel; 2] = [Pixel::YUV420P, Pixel::NV12];

/// Hardware H.264 encoders to try when the caller opts into hardware encode,
/// best quality / throughput first.
#[cfg(target_os = "macos")]
const HARDWARE_H264: &[&str] = &["h264_videotoolbox"];
#[cfg(target_os = "windows")]
const HARDWARE_H264: &[&str] = &["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf"];
#[cfg(target_os = "linux")]
const HARDWARE_H264: &[&str] = &["h264_nvenc", "h264_vaapi", "h264_qsv"];
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
const HARDWARE_H264: &[&str] = &[];

/// Encoders to fall back to when FFmpeg has **no** software H.264 (common on
/// LGPL Windows builds that drop GPL libx264). Ordered for *openability* on an
/// arbitrary machine rather than raw quality: on Windows, Media Foundation
/// opens on any GPU — or on the CPU — whereas vendor encoders are present in
/// the build yet fail to open without their specific hardware.
#[cfg(target_os = "windows")]
const FALLBACK_H264: &[&str] = &["h264_mf", "h264_nvenc", "h264_amf", "h264_qsv"];
#[cfg(not(target_os = "windows"))]
const FALLBACK_H264: &[&str] = HARDWARE_H264;

/// The most-preferred software input format `codec` advertises, or `None` if it
/// only takes hardware surfaces (e.g. `h264_d3d12va` accepts just `d3d12`,
/// `h264_qsv` just `qsv`). Encoders that advertise no pixel-format list at all
/// are assumed to take the classic planar default.
fn software_input_format(codec: Codec) -> Option<Pixel> {
    let video = codec.video().ok()?;
    match video.formats() {
        Some(formats) => {
            let supported: Vec<Pixel> = formats.collect();
            SOFTWARE_INPUT_FORMATS
                .into_iter()
                .find(|fmt| supported.contains(fmt))
        }
        None => Some(Pixel::YUV420P),
    }
}

fn as_software_encoder(codec: Codec) -> Option<H264Encoder> {
    software_input_format(codec).map(|pixel| H264Encoder { codec, pixel })
}

fn named_software_encoder(name: &str) -> Option<H264Encoder> {
    encoder::find_by_name(name).and_then(as_software_encoder)
}

fn first_software_encoder(names: &[&str]) -> Option<H264Encoder> {
    names.iter().copied().find_map(named_software_encoder)
}

/// Pick an H.264 encoder and the software pixel format to feed it.
///
/// Order: requested hardware encoder (if any) → software libx264/libopenh264 →
/// a CPU-frame-capable hardware encoder → the build's default H.264 encoder.
/// A hardware-surface-only encoder is *never* returned for our software-frame
/// pipeline — that would surface as a confusing "failed to open media" when the
/// encoder rejects the `yuv420p` frames at open time.
pub(crate) fn find_h264_encoder(hardware: bool) -> Option<H264Encoder> {
    if hardware && let Some(enc) = first_software_encoder(HARDWARE_H264) {
        return Some(enc);
    }

    // Software encoders: cleanest deliverable, deterministic, always YUV420P.
    if let Some(enc) = first_software_encoder(&["libx264", "libopenh264"]) {
        return Some(enc);
    }

    // No software H.264 in this build: rather than fail the export, accept a
    // hardware encoder that still takes CPU frames (Media Foundation, NVENC, …).
    if let Some(enc) = first_software_encoder(FALLBACK_H264) {
        return Some(enc);
    }

    // Last resort: whatever the build registered for H.264 — but only if it can
    // take software frames, never a hardware-surface-only encoder.
    encoder::find(codec::Id::H264).and_then(as_software_encoder)
}

pub(crate) fn is_eagain(e: &FfmpegError) -> bool {
    matches!(e, FfmpegError::Other { errno } if *errno == EAGAIN)
}

/// Target dimensions: preserve aspect ratio, never upscale, round to even.
/// Proxy-only policy — export sizing honors the user's pick exactly
/// (`export_dims` in cutlass-engine).
pub(crate) fn scaled_dims(src_w: u32, src_h: u32, target_h: u32) -> (u32, u32) {
    if src_h == 0 {
        return (src_w.max(2) & !1, src_h);
    }
    let h = target_h.clamp(2, src_h);
    let w = ((src_w as u64 * h as u64) / src_h as u64) as u32;
    (w.max(2) & !1, h.max(2) & !1)
}

/// Fill a YUV420P frame from tight plane buffers (GPU readback layout).
pub(crate) fn fill_yuv420p_frame(
    frame: &mut VideoFrame,
    width: u32,
    height: u32,
    y: &[u8],
    u: &[u8],
    v: &[u8],
) -> Result<(), EncodeError> {
    let w = width as usize;
    let h = height as usize;
    let y_need = w * h;
    let uv_need = (w / 2) * (h / 2);
    if y.len() != y_need || u.len() != uv_need || v.len() != uv_need {
        return Err(EncodeError::unsupported(format!(
            "yuv plane sizes mismatch for {width}x{height}"
        )));
    }

    let y_stride = frame.stride(0);
    let u_stride = frame.stride(1);
    let v_stride = frame.stride(2);
    for row in 0..h {
        let dst = &mut frame.data_mut(0)[row * y_stride..row * y_stride + w];
        let src = &y[row * w..row * w + w];
        dst.copy_from_slice(src);
    }
    let uv_h = h / 2;
    let uv_w = w / 2;
    for row in 0..uv_h {
        let dst = &mut frame.data_mut(1)[row * u_stride..row * u_stride + uv_w];
        let src = &u[row * uv_w..row * uv_w + uv_w];
        dst.copy_from_slice(src);
    }
    for row in 0..uv_h {
        let dst = &mut frame.data_mut(2)[row * v_stride..row * v_stride + uv_w];
        let src = &v[row * uv_w..row * uv_w + uv_w];
        dst.copy_from_slice(src);
    }
    Ok(())
}

/// Fill a reusable RGBA frame from a row-major RGBA8 buffer.
pub(crate) fn fill_rgba_frame(
    frame: &mut VideoFrame,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), EncodeError> {
    let expected = (width as usize) * (height as usize) * 4;
    if rgba.len() != expected {
        return Err(EncodeError::unsupported(format!(
            "rgba buffer is {} bytes, expected {expected}",
            rgba.len()
        )));
    }

    let row_bytes = (width as usize) * 4;
    let stride = frame.stride(0);
    let data = frame.data_mut(0);
    for y in 0..height as usize {
        let dst = &mut data[y * stride..y * stride + row_bytes];
        let src = &rgba[y * row_bytes..y * row_bytes + row_bytes];
        dst.copy_from_slice(src);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core regression guard: on a build whose only H.264 encoders take
    /// hardware surfaces (e.g. Windows LGPL FFmpeg with just `h264_d3d12va`),
    /// we must never hand the software-frame pipeline such an encoder — it
    /// rejects `yuv420p` at open time and surfaces as "failed to open media".
    #[test]
    fn selected_encoder_always_takes_software_frames() {
        ensure_ffmpeg_init().expect("ffmpeg init");
        for hardware in [false, true] {
            if let Some(enc) = find_h264_encoder(hardware) {
                assert!(
                    SOFTWARE_INPUT_FORMATS.contains(&enc.pixel),
                    "{} selected non-software pixel {:?}",
                    enc.codec.name(),
                    enc.pixel,
                );
                assert_eq!(
                    software_input_format(enc.codec),
                    Some(enc.pixel),
                    "{} pixel choice disagrees with advertised formats",
                    enc.codec.name(),
                );
            }
        }
    }

    /// When the GPL software encoder is present (CI/dev), the software path must
    /// prefer it at constant quality — never silently degrade to a hardware one.
    #[test]
    fn software_path_prefers_libx264_when_present() {
        ensure_ffmpeg_init().expect("ffmpeg init");
        if encoder::find_by_name("libx264").is_some() {
            let enc = find_h264_encoder(false).expect("an H.264 encoder");
            assert_eq!(enc.codec.name(), "libx264");
            assert_eq!(enc.pixel, Pixel::YUV420P);
        }
    }
}
