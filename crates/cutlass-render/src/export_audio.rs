//! Export-side audio: collect every audible clip and mix it, streamed, into
//! interleaved stereo `f32` blocks for the encoder's AAC track.
//!
//! Decodes straight from the original source files (same rule as export video:
//! no cache, no proxies). The mix policy is the MVP subset of the desktop
//! mixer — sum overlapping spans, apply per-sample volume + fades, clamp to
//! `[-1, 1]`, silence where nothing is audible. It is fail-loud: a source that
//! can't be opened or read aborts the export.
//!
//! Retimed clips (speed / speed-curve ramps) are rendered with varispeed
//! resampling so pitch follows playback rate; pitch-preserving time-stretch
//! is deferred. Denoise-flagged clips run through RNNoise before mixing.
//! Reversed clips still export silent (forward-only decoders).
//!
//! The export loop drives [`ExportAudioMixer::mix_into`] with monotonically
//! advancing positions (one block per video frame), so each span's reader seeks
//! once at its in-point and then streams sequentially.

use std::path::PathBuf;

use cutlass_core::{AudioReader, DecodeError};
use cutlass_models::{Param, Project, audio_gain_at};

use crate::audio_dsp::{DenoiseReader, SpanWarp, warped_source_frame};

/// Export audio sample rate: the broadcast/web standard for video files.
pub const EXPORT_AUDIO_RATE: u32 = 48_000;
/// Export channel count (interleaved stereo).
pub const EXPORT_AUDIO_CHANNELS: u16 = 2;

const CHANNELS: usize = EXPORT_AUDIO_CHANNELS as usize;

/// One audible clip resolved to output sample frames at [`EXPORT_AUDIO_RATE`].
struct Span {
    path: PathBuf,
    /// Timeline placement in output sample frames.
    start: i64,
    end: i64,
    /// Source position (output sample frames) of the span's first sample.
    source_start: i64,
    /// How output samples map into the source window.
    warp: SpanWarp,
    /// Run RNNoise on this clip's audio before mixing.
    denoise: bool,
    /// Clip gain envelope, ticks rebased to clip-relative output sample frames.
    volume: Param<f32>,
    /// Fade ramp lengths in output sample frames, anchored at the span edges.
    fade_in: i64,
    fade_out: i64,
    /// Opened on first overlap, dropped with the mixer.
    reader: Option<Box<dyn AudioReader>>,
    /// Source ran out before the span's out-point: the rest pads as silence.
    exhausted: bool,
}

/// Streamed mixer over every audible span of a project's timeline.
pub struct ExportAudioMixer {
    spans: Vec<Span>,
    scratch: Vec<f32>,
    warp_scratch: Vec<f32>,
}

impl ExportAudioMixer {
    /// Audible spans: clips on unmuted lanes whose media carries an audio
    /// stream. CapCut-style, a video clip keeps its own sound, so video lanes
    /// are audible too; only a clip detached to a linked audio lane defers its
    /// audio there. `None` when the timeline is silent, so callers can skip the
    /// audio track entirely.
    ///
    /// Reversed clips are skipped (forward-only decoders cannot stream backward
    /// efficiently); all other retimed and denoised clips are mixed.
    pub fn for_project(project: &Project) -> Option<Self> {
        let timeline = project.timeline();
        let fps = timeline.frame_rate;
        let mut spans = Vec::new();
        for track in timeline.tracks_ordered() {
            if track.muted {
                continue;
            }
            for clip in track.clips_ordered() {
                if clip.is_silent() {
                    continue;
                }
                if clip.reversed {
                    continue;
                }
                if !timeline.carries_own_audio(clip.id) {
                    continue;
                }
                let Some(media_id) = clip.media() else {
                    continue;
                };
                let Some(media) = project.media(media_id) else {
                    continue;
                };
                if !media.has_audio {
                    continue;
                };
                let Some(source) = clip.source_range() else {
                    continue;
                };
                let warp = if clip.has_speed_curve() {
                    SpanWarp::Curved {
                        curve: clip.speed_curve.clone(),
                        source_len: ticks_to_samples(
                            source.duration.value,
                            source.start.rate.num,
                            source.start.rate.den,
                        ),
                        curve_total: clip.speed_curve_average(),
                    }
                } else if clip.speed.num != clip.speed.den {
                    SpanWarp::FlatSpeed {
                        num: clip.speed.num,
                        den: clip.speed.den,
                    }
                } else {
                    SpanWarp::Linear
                };
                spans.push(Span {
                    path: media.path().to_path_buf(),
                    start: ticks_to_samples(clip.timeline.start.value, fps.num, fps.den),
                    end: ticks_to_samples(clip.timeline.end_tick(), fps.num, fps.den),
                    source_start: ticks_to_samples(
                        source.start.value,
                        source.start.rate.num,
                        source.start.rate.den,
                    ),
                    warp,
                    denoise: clip.denoise,
                    volume: clip
                        .volume
                        .map_ticks(|tick| ticks_to_samples(tick, fps.num, fps.den)),
                    fade_in: ticks_to_samples(clip.fade_in, fps.num, fps.den),
                    fade_out: ticks_to_samples(clip.fade_out, fps.num, fps.den),
                    reader: None,
                    exhausted: false,
                });
            }
        }
        if spans.is_empty() {
            None
        } else {
            Some(Self {
                spans,
                scratch: Vec::new(),
                warp_scratch: Vec::new(),
            })
        }
    }

    /// Mix every span overlapping `[pos, pos + out.len()/2)` into `out`
    /// (interleaved stereo; cleared to silence first).
    pub fn mix_into(&mut self, pos: i64, out: &mut [f32]) -> Result<(), DecodeError> {
        out.fill(0.0);
        let block_frames = (out.len() / CHANNELS) as i64;
        let block_end = pos + block_frames;

        for i in 0..self.spans.len() {
            let span = &self.spans[i];
            if span.start >= block_end || span.end <= pos || span.exhausted {
                continue;
            }
            let s = span.start.max(pos);
            let e = span.end.min(block_end);
            if matches!(span.warp, SpanWarp::Linear) {
                Self::mix_linear_span(&mut self.spans[i], &mut self.scratch, s, e, pos, out)?;
            } else {
                Self::mix_warped_span(
                    &mut self.spans[i],
                    &mut self.scratch,
                    &mut self.warp_scratch,
                    s,
                    e,
                    pos,
                    out,
                )?;
            }
        }

        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
        Ok(())
    }

    fn mix_linear_span(
        span: &mut Span,
        scratch: &mut Vec<f32>,
        s: i64,
        e: i64,
        pos: i64,
        out: &mut [f32],
    ) -> Result<(), DecodeError> {
        let reader = match &mut span.reader {
            Some(reader) => reader,
            None => span.reader.insert(open_span_reader(span)?),
        };

        let src_from = span.source_start + (s - span.start);
        reader
            .seek_to_frame(src_from)
            .map_err(|err| audio_err("seek audio source", &span.path, err))?;
        let lead = reader
            .position()
            .map_or(0, |p| (p - src_from).clamp(0, e - s));

        let want = ((e - s) - lead) as usize;
        if want == 0 {
            return Ok(());
        }
        scratch.resize(want * CHANNELS, 0.0);
        let got = reader
            .read(&mut scratch[..want * CHANNELS])
            .map_err(|err| audio_err("decode audio source", &span.path, err))?;
        if got < want {
            span.exhausted = true;
        }

        let offset = ((s - pos + lead) as usize) * CHANNELS;
        accumulate_span_samples(span, s + lead, got, offset, out, scratch);
        Ok(())
    }

    fn mix_warped_span(
        span: &mut Span,
        scratch: &mut Vec<f32>,
        warp_scratch: &mut Vec<f32>,
        s: i64,
        e: i64,
        pos: i64,
        out: &mut [f32],
    ) -> Result<(), DecodeError> {
        let reader = match &mut span.reader {
            Some(reader) => reader,
            None => span.reader.insert(open_span_reader(span)?),
        };

        let span_len = span.end - span.start;
        let rel_start = s - span.start;
        let rel_end = e - span.start;
        let out_frames = (rel_end - rel_start) as usize;
        if out_frames == 0 {
            return Ok(());
        }

        let mut src_min = f64::MAX;
        let mut src_max = f64::MIN;
        for rel in rel_start..rel_end {
            let src = warped_source_frame(&span.warp, rel, span_len);
            src_min = src_min.min(src);
            src_max = src_max.max(src);
        }

        let src_floor = src_min.floor() as i64;
        let src_ceil = src_max.ceil() as i64 + 1;
        let need = (src_ceil - src_floor).max(0) as usize;
        if need == 0 {
            return Ok(());
        }

        reader
            .seek_to_frame(span.source_start + src_floor)
            .map_err(|err| audio_err("seek audio source", &span.path, err))?;
        warp_scratch.resize(need * CHANNELS, 0.0);
        let got = reader
            .read(&mut warp_scratch[..need * CHANNELS])
            .map_err(|err| audio_err("decode audio source", &span.path, err))?;
        if (got as i64) < src_ceil - src_floor {
            span.exhausted = true;
        }

        scratch.resize(out_frames * CHANNELS, 0.0);
        for (out_idx, rel) in (rel_start..rel_end).enumerate() {
            let src = warped_source_frame(&span.warp, rel, span_len);
            let src_rel = src - src_floor as f64;
            let base = src_rel.floor() as usize;
            let frac = src_rel - base as f64;
            if base >= got {
                continue;
            }
            for ch in 0..CHANNELS {
                let a = warp_scratch[base * CHANNELS + ch];
                let b = if base + 1 < got {
                    warp_scratch[(base + 1) * CHANNELS + ch]
                } else {
                    a
                };
                scratch[out_idx * CHANNELS + ch] = a + (b - a) * frac as f32;
            }
        }

        let offset = ((s - pos) as usize) * CHANNELS;
        accumulate_span_samples(span, s, out_frames, offset, out, scratch);
        Ok(())
    }
}

fn open_span_reader(span: &Span) -> Result<Box<dyn AudioReader>, DecodeError> {
    let reader =
        cutlass_decoder::open_audio_reader(&span.path, EXPORT_AUDIO_RATE, EXPORT_AUDIO_CHANNELS)
            .map_err(|err| audio_err("open audio source", &span.path, err))?;
    if span.denoise {
        Ok(Box::new(DenoiseReader::new(reader, CHANNELS)))
    } else {
        Ok(reader)
    }
}

fn accumulate_span_samples(
    span: &Span,
    span_rel_start: i64,
    frames: usize,
    out_offset: usize,
    out: &mut [f32],
    samples: &[f32],
) {
    let span_len = span.end - span.start;
    let first = span_rel_start - span.start;
    let unity = span.volume.constant() == Some(1.0) && span.fade_in == 0 && span.fade_out == 0;
    if unity {
        for (dst, src) in out[out_offset..]
            .iter_mut()
            .zip(&samples[..frames * CHANNELS])
        {
            *dst += *src;
        }
    } else {
        for frame in 0..frames {
            let gain = audio_gain_at(
                first + frame as i64,
                span_len,
                &span.volume,
                span.fade_in,
                span.fade_out,
            );
            for ch in 0..CHANNELS {
                out[out_offset + frame * CHANNELS + ch] += samples[frame * CHANNELS + ch] * gain;
            }
        }
    }
}

fn audio_err(what: &str, path: &std::path::Path, err: impl std::fmt::Display) -> DecodeError {
    DecodeError::Decode(format!("{what} {}: {err}", path.display()))
}

/// `value` ticks at `num/den` fps → sample frames at the export rate (exact
/// i128, floored) — the same conversion the desktop mixer uses.
fn ticks_to_samples(value: i64, num: i32, den: i32) -> i64 {
    if num <= 0 || den <= 0 {
        return 0;
    }
    let frames =
        i128::from(value) * i128::from(den) * i128::from(EXPORT_AUDIO_RATE) / i128::from(num);
    frames.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Sample-frame boundary of output video frame `n` at `out_num/out_den` fps:
/// the export loop pushes audio block `[boundary(n), boundary(n+1))` after video
/// frame `n`, so audio and video cover identical wall-clock spans.
pub fn sample_boundary(n: i64, out_num: i32, out_den: i32) -> i64 {
    ticks_to_samples(n, out_num, out_den)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_dsp::SpanWarp;
    use cutlass_core::AudioReader;
    use cutlass_models::Rational;
    use std::f32::consts::PI;

    struct SineReader {
        pos: i64,
        rate: u32,
        freq: f32,
    }

    impl AudioReader for SineReader {
        fn read(&mut self, out: &mut [f32]) -> Result<usize, DecodeError> {
            let frames = out.len() / CHANNELS;
            for f in 0..frames {
                let t = (self.pos + f as i64) as f32 / self.rate as f32;
                let sample = (2.0 * PI * self.freq * t).sin();
                for ch in 0..CHANNELS {
                    out[f * CHANNELS + ch] = sample;
                }
            }
            self.pos += frames as i64;
            Ok(frames)
        }

        fn seek_to_frame(&mut self, frame: i64) -> Result<(), DecodeError> {
            self.pos = frame;
            Ok(())
        }

        fn position(&self) -> Option<i64> {
            Some(self.pos)
        }
    }

    #[test]
    fn ticks_to_samples_is_exact_for_common_rates() {
        assert_eq!(ticks_to_samples(24, 24, 1), 48_000);
        assert_eq!(ticks_to_samples(1, 24, 1), 2_000);
        assert_eq!(ticks_to_samples(30_000, 30_000, 1_001), 1_001 * 48_000);
    }

    #[test]
    fn sample_boundaries_partition_the_stream() {
        assert_eq!(sample_boundary(0, 24, 1), 0);
        assert_eq!(sample_boundary(1, 24, 1), 2_000);
        assert_eq!(sample_boundary(48, 24, 1), 96_000);
        let mut prev = 0;
        for n in 1..=100 {
            let b = sample_boundary(n, 30_000, 1_001);
            assert!(b > prev, "boundaries advance");
            prev = b;
        }
    }

    #[test]
    fn silent_project_has_no_mixer() {
        let project = Project::new("test", Rational::FPS_24);
        assert!(ExportAudioMixer::for_project(&project).is_none());
    }

    #[test]
    fn flat_speed_warp_maps_endpoints() {
        let warp = SpanWarp::FlatSpeed { num: 2, den: 1 };
        assert!((warped_source_frame(&warp, 0, 2000) - 0.0).abs() < 1e-6);
        assert!((warped_source_frame(&warp, 2000, 2000) - 4000.0).abs() < 1e-6);
    }

    #[test]
    fn warped_mix_reads_double_source_for_2x_speed() {
        let mut mixer = ExportAudioMixer {
            spans: vec![Span {
                path: PathBuf::from("/dev/null"),
                start: 0,
                end: 1000,
                source_start: 0,
                warp: SpanWarp::FlatSpeed { num: 2, den: 1 },
                denoise: false,
                volume: Param::Constant(1.0),
                fade_in: 0,
                fade_out: 0,
                reader: Some(Box::new(SineReader {
                    pos: 0,
                    rate: EXPORT_AUDIO_RATE,
                    freq: 440.0,
                })),
                exhausted: false,
            }],
            scratch: Vec::new(),
            warp_scratch: Vec::new(),
        };
        let mut out = vec![0.0; 500 * CHANNELS];
        mixer.mix_into(0, &mut out).unwrap();
        assert!(out.iter().any(|&s| s.abs() > 0.01));
        let reader_pos = mixer.spans[0].reader.as_ref().unwrap().position().unwrap();
        assert!(
            reader_pos >= 998,
            "2× speed should advance source ~twice as fast, got {reader_pos}"
        );
    }
}
