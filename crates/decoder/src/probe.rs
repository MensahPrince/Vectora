//! Lightweight read-only stream metadata extraction.
//!
//! Used by the import path: probe a file just enough to populate
//! [`models::MediaSource`] (kind, dimensions, fps, sample rate, codecs,
//! container-reported duration). We deliberately **do not** open the codec —
//! [`crate::Decoder::open`] is the full-fat path used by playback, and is
//! ~10× more expensive (codec init, threading config, pixel-format validation).
//!
//! All values come from `AVCodecParameters` populated by the demuxer, plus
//! container-level metadata on the `AVFormatContext`. The probe is therefore
//! safe to call from any thread (each call gets its own input context), and
//! finishes in single-digit milliseconds for local files.
//!
//! # Threading
//! Reentrant. Calling code chooses whether to run probes serially (e.g. on
//! the import worker) or in parallel — ffmpeg's libavformat is happy either
//! way as long as each `AVFormatContext` stays owned by one thread, which
//! [`probe`] guarantees by construction (the context is local to the call).

use std::ffi::CStr;
use std::path::Path;

use ffmpeg_next::format::{self, context::Input};
use ffmpeg_next::media::Type;

use crate::decoder::ensure_ffmpeg_init;
use crate::error::DecoderError;
use crate::time::Rational;

/// Container-level summary of a media file.
///
/// `duration` is the *container* duration (best-effort: some formats only
/// report per-stream durations, in which case this is `None` and the caller
/// can fall back on the longest stream when needed).
#[derive(Debug, Clone)]
pub struct ProbedSource {
    pub kind: ProbedKind,
    pub duration: Option<Rational>,
    pub video: Option<ProbedVideo>,
    pub audio: Option<ProbedAudio>,
}

/// What the file "is" from a user-facing perspective. A `.png` is reported as
/// `Image` even though it ships through ffmpeg's video pipeline as a 1-frame
/// MJPEG/PNG stream; the import UI needs to render it as a thumbnail tile,
/// not a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbedKind {
    Video,
    Audio,
    Image,
}

#[derive(Debug, Clone)]
pub struct ProbedVideo {
    pub width: u32,
    pub height: u32,
    /// Container-reported `avg_frame_rate`. `Rational::new_raw(0, 1)` when the
    /// container doesn't report one (e.g. variable-fps webm without index).
    pub fps: Rational,
    /// FFmpeg short codec name (`"h264"`, `"hevc"`, `"vp9"`, …). Empty string
    /// only when ffmpeg has no descriptor for the codec ID.
    pub codec: String,
}

#[derive(Debug, Clone)]
pub struct ProbedAudio {
    pub sample_rate: u32,
    /// FFmpeg short codec name (`"aac"`, `"opus"`, `"pcm_s16le"`, …).
    pub codec: String,
}

/// Probe a single file. Synchronous; safe to call from a worker thread.
///
/// Returns `Err(DecoderError::Unsupported)` if the file doesn't contain any
/// usable video or audio stream — bubble that up as the `MediaSource.error`
/// so the user sees a red tile instead of a silent drop.
pub fn probe(path: &Path) -> Result<ProbedSource, DecoderError> {
    ensure_ffmpeg_init()?;

    let path_str = path
        .to_str()
        .ok_or_else(|| DecoderError::unsupported("path is not valid UTF-8"))?;

    let input = format::input(path_str).map_err(DecoderError::Open)?;

    let video = probe_video(&input);
    let audio = probe_audio(&input);

    if video.is_none() && audio.is_none() {
        return Err(DecoderError::unsupported(
            "no usable video or audio streams",
        ));
    }

    let kind = if video.is_some() {
        if is_image_format(input.format().name()) {
            ProbedKind::Image
        } else {
            ProbedKind::Video
        }
    } else {
        ProbedKind::Audio
    };

    let duration = container_duration(&input);

    Ok(ProbedSource {
        kind,
        duration,
        video,
        audio,
    })
}

fn probe_video(input: &Input) -> Option<ProbedVideo> {
    let stream = input.streams().best(Type::Video)?;
    let params = stream.parameters();

    // SAFETY: `params.as_ptr()` returns a non-null `*const AVCodecParameters`
    // owned by the demuxer; lifetime is bounded by `input` (kept alive across
    // this borrow). We read only POD scalar fields, no string/buffer access.
    let (codec_id, width, height) = unsafe {
        let p = &*params.as_ptr();
        (p.codec_id, p.width, p.height)
    };

    if width <= 0 || height <= 0 {
        return None;
    }

    let codec = codec_name(codec_id);
    let fps = avg_frame_rate(&stream);

    Some(ProbedVideo {
        width: width as u32,
        height: height as u32,
        fps,
        codec,
    })
}

fn probe_audio(input: &Input) -> Option<ProbedAudio> {
    let stream = input.streams().best(Type::Audio)?;
    let params = stream.parameters();

    // SAFETY: see `probe_video`; same contract.
    let (codec_id, sample_rate) = unsafe {
        let p = &*params.as_ptr();
        (p.codec_id, p.sample_rate)
    };

    if sample_rate <= 0 {
        return None;
    }

    Some(ProbedAudio {
        sample_rate: sample_rate as u32,
        codec: codec_name(codec_id),
    })
}

fn codec_name(id: ffmpeg_next::ffi::AVCodecID) -> String {
    // SAFETY: `avcodec_get_name` either returns a pointer to a static C
    // string ("h264", "aac", …) or to a static fallback string. Never null
    // for valid IDs; we still guard against null defensively.
    unsafe {
        let raw = ffmpeg_next::ffi::avcodec_get_name(id);
        if raw.is_null() {
            String::new()
        } else {
            CStr::from_ptr(raw).to_string_lossy().into_owned()
        }
    }
}

fn avg_frame_rate(stream: &ffmpeg_next::format::stream::Stream<'_>) -> Rational {
    let r = stream.avg_frame_rate();
    let num = r.numerator();
    let den = r.denominator();
    // Many containers (still-image pipes, malformed streams) report 0/0 here.
    // Surface that as "unknown" rather than panicking on a zero denominator.
    if num > 0 && den > 0 {
        Rational::new(num as i64, den as u32).unwrap_or(Rational::new_raw(0, 1))
    } else {
        Rational::new_raw(0, 1)
    }
}

fn container_duration(input: &Input) -> Option<Rational> {
    // `Input::duration()` returns ticks in AV_TIME_BASE (1/1_000_000 s).
    // `AV_NOPTS_VALUE` is `0x8000_0000_0000_0000` as i64 (i.e. i64::MIN).
    let micros = input.duration();
    if micros <= 0 || micros == i64::MIN {
        return None;
    }
    Rational::new(micros, 1_000_000)
}

/// Heuristic: containers that demux a single still image as a 1-frame video
/// stream. Anything that comes through one of these demuxers is rendered as
/// an image tile in the library, not a clip.
fn is_image_format(name: &str) -> bool {
    // ffmpeg demuxer names use commas to separate aliases for one demuxer,
    // e.g. `"mov,mp4,m4a,3gp,3g2,mj2"`. Image demuxers don't, so a direct
    // contains-check on the comma-split list is sufficient.
    name.split(',').any(|n| {
        matches!(
            n.trim(),
            "image2"
                | "image2pipe"
                | "png_pipe"
                | "mjpeg"
                | "mjpeg_2000"
                | "jpeg_pipe"
                | "jpegls_pipe"
                | "webp_pipe"
                | "bmp_pipe"
                | "tiff_pipe"
                | "gif"
                | "psd_pipe"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_format_matches_known_pipes() {
        assert!(is_image_format("png_pipe"));
        assert!(is_image_format("image2"));
        assert!(is_image_format("jpeg_pipe"));
        assert!(is_image_format("webp_pipe"));
        // Single GIF is an image; animated GIF still routes through this
        // demuxer and is treated as Image in v1 (acceptable: the engine
        // doesn't yet distinguish animated stills).
        assert!(is_image_format("gif"));
    }

    #[test]
    fn image_format_rejects_real_video_containers() {
        assert!(!is_image_format("mov,mp4,m4a,3gp,3g2,mj2"));
        assert!(!is_image_format("matroska,webm"));
        assert!(!is_image_format("avi"));
        assert!(!is_image_format("wav"));
        assert!(!is_image_format("flac"));
    }

    #[test]
    fn probe_rejects_missing_file() {
        let err = probe(Path::new("/definitely/does/not/exist.mp4")).unwrap_err();
        // Demuxer failure → DecoderError::Open. We don't pin the inner
        // ffmpeg error code (varies by platform) — just the variant.
        assert!(matches!(err, DecoderError::Open(_)));
    }
}
