//! Shared low-rate mono decode for the audio-analysis edits.
//!
//! Silence removal, caption transcription, and transcript editing all need the
//! same thing first: a clip's source window decoded to a flat `Vec<f32>` of
//! mono samples at a low analysis rate (16 kHz — all speech/level analysis
//! needs, and far cheaper than export rate). They differ only in what they do
//! with the samples (DSP vs. a transcribe backend), so the decode lives here
//! once instead of being copied per feature.
//!
//! Streams in blocks so memory stays bounded, and rejects generated clips and
//! media without audio with a generic message (the agent-facing rejections with
//! feature-specific wording live in `cutlass-ai`'s validation layer).

use cutlass_decoder::{AUDIO_CHANNELS, AudioReader};
use cutlass_models::{ClipId, ModelError, Project};

use crate::error::EngineError;

/// Decode rate for audio analysis. whisper-class models expect 16 kHz mono, and
/// level/silence analysis needs no more, so we decode far less than export rate.
pub(crate) const ANALYSIS_RATE: u32 = 16_000;

/// Decode a clip's source window at [`ANALYSIS_RATE`], downmixed to mono.
/// Rejects generated clips and media without audio. Returns an empty vec for a
/// zero-length source window.
pub(crate) fn decode_clip_mono(project: &Project, clip_id: ClipId) -> Result<Vec<f32>, EngineError> {
    let clip = project.clip(clip_id).ok_or(ModelError::UnknownClip(clip_id))?;
    let media_id = clip
        .media()
        .ok_or_else(|| ModelError::InvalidParam("audio analysis requires a media clip".into()))?;
    let media = project
        .media(media_id)
        .ok_or(ModelError::UnknownMedia(media_id))?;
    if !media.has_audio {
        return Err(ModelError::InvalidParam("clip media has no audio to analyze".into()).into());
    }
    let source = clip
        .source_range()
        .ok_or_else(|| ModelError::InvalidParam("audio analysis requires a media clip".into()))?;
    let src_start = ticks_to_samples(source.start.value, source.start.rate);
    let src_frames = ticks_to_samples(source.duration.value, source.start.rate);
    if src_frames <= 0 {
        return Ok(Vec::new());
    }
    read_mono(media.path(), src_start, src_frames)
}

/// Read `frames` output frames from `src_start` of `path` at [`ANALYSIS_RATE`],
/// downmixed to mono. Streams in blocks so memory stays bounded.
fn read_mono(
    path: &std::path::Path,
    src_start: i64,
    frames: i64,
) -> Result<Vec<f32>, EngineError> {
    const BLOCK: usize = 16_384;
    let mut reader = AudioReader::open(path, ANALYSIS_RATE)?;
    reader.seek_to_frame(src_start)?;
    let mut mono = Vec::with_capacity(frames as usize);
    let mut buf = vec![0.0f32; BLOCK * AUDIO_CHANNELS];
    let mut remaining = frames as usize;
    while remaining > 0 {
        let want = remaining.min(BLOCK);
        let got = reader.read(&mut buf[..want * AUDIO_CHANNELS])?;
        if got == 0 {
            break;
        }
        for f in 0..got {
            mono.push((buf[f * AUDIO_CHANNELS] + buf[f * AUDIO_CHANNELS + 1]) * 0.5);
        }
        remaining -= got;
    }
    Ok(mono)
}

/// `value` ticks at `fps` → output frames at [`ANALYSIS_RATE`] (exact i128,
/// floored).
fn ticks_to_samples(value: i64, fps: cutlass_models::Rational) -> i64 {
    if fps.num <= 0 || fps.den <= 0 {
        return 0;
    }
    let frames =
        i128::from(value) * i128::from(fps.den) * i128::from(ANALYSIS_RATE) / i128::from(fps.num);
    frames.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}
