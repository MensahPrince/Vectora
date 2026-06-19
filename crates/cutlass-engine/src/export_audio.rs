//! Export-side audio: collect every audible clip and mix it, streamed, into
//! interleaved stereo f32 blocks for the encoder's AAC track.
//!
//! Decodes straight from the original source files (same rule as export
//! video: no cache, no proxies). The mix policy mirrors the preview mixer —
//! sum overlapping spans, clamp to [-1, 1], silence where nothing is audible
//! — but unlike preview it is fail-loud: a source that cannot be opened or
//! read aborts the export, because a deliverable with silently missing audio
//! is worse than an error.
//!
//! The export loop drives [`ExportAudioMixer::mix_into`] with monotonically
//! advancing positions (one block per video frame), so each span's reader
//! seeks once at its in-point and then streams sequentially.

use std::path::PathBuf;

use cutlass_decoder::{AUDIO_CHANNELS, AudioReader};
use cutlass_models::{Param, Project, audio_gain_at};

use crate::error::EngineError;

/// Export audio sample rate: the broadcast/web standard for video files.
pub const EXPORT_AUDIO_RATE: u32 = 48_000;

/// One audible clip resolved to output sample frames at [`EXPORT_AUDIO_RATE`].
struct Span {
    path: PathBuf,
    /// Timeline placement in output sample frames.
    start: i64,
    end: i64,
    /// Source position (in output sample frames) of the span's first sample.
    source_start: i64,
    /// Source window length in output sample frames (the clip's in/out range).
    /// Drives the varispeed stretch ratio when `retimed`.
    source_frames: i64,
    /// Retimed (constant speed ≠ 1×, reversed, and/or a speed ramp, M8 Phase
    /// 3): the span is time-stretched to its timeline length and served 1:1,
    /// not read at native rate.
    retimed: bool,
    /// Play the source window back-to-front (CapCut reverse).
    reversed: bool,
    /// Varispeed pitch factor (`1.0` keeps pitch, `> 1.0` is chipmunk mode).
    pitch_factor: f32,
    /// Speed ramp (CapCut speed curves, M2): `Some` ⇒ the retime rate varies,
    /// and the stretch follows the curve's normalized integral. `None` ⇒ a
    /// constant-rate retime.
    speed_curve: Option<Param<f32>>,
    /// Clip gain envelope (volume, M1 → M8): `1.0` ⇔ unchanged. Keyframe
    /// ticks are rebased into clip-relative output sample frames.
    volume: Param<f32>,
    /// Fade ramp lengths in output sample frames, anchored at the span edges.
    fade_in: i64,
    fade_out: i64,
    /// Noise reduction (M8 Phase 5): render the span's audio through RNNoise
    /// and serve the cleaned buffer (`rendered`), like a retime. Stacks with a
    /// retime (denoise rides on top of the stretched buffer).
    denoise: bool,
    /// Opened on first overlap, dropped with the mixer. Unused for rendered
    /// spans (retimed and/or denoised), which serve from `rendered` instead.
    reader: Option<AudioReader>,
    /// Rendered buffer for a retimed and/or denoised span, produced fail-loud
    /// on first overlap (interleaved stereo, `out_frames * CHANNELS` long).
    rendered: Option<Vec<f32>>,
    /// Source ran out before the span's out-point: the rest pads as silence.
    exhausted: bool,
}

/// Streamed mixer over every audible span of a project's timeline.
pub struct ExportAudioMixer {
    spans: Vec<Span>,
    scratch: Vec<f32>,
}

impl ExportAudioMixer {
    /// Audible spans: clips on unmuted lanes whose media carries an audio
    /// stream. CapCut-style, a video clip keeps its own sound, so video lanes
    /// are audible too; only a clip detached to a linked audio-lane partner
    /// (CapCut "separate audio") defers its audio there. Mirrors the preview
    /// mixer's `audio_snapshot`. `None` when the timeline is silent, so callers
    /// can skip the audio track entirely.
    pub fn for_project(project: &Project) -> Option<Self> {
        let timeline = project.timeline();
        let fps = timeline.frame_rate;
        let mut spans = Vec::new();
        for track in timeline.tracks_ordered() {
            if track.muted {
                continue;
            }
            for clip in track.clips_ordered() {
                // Constant-zero clips contribute nothing. Every retimed clip —
                // constant speed, reverse, and now speed ramps (M2) —
                // time-stretches (M8 Phase 3) so export matches the preview
                // mixer.
                if clip.is_silent() {
                    continue;
                }
                // Audio rides the clip's own lane unless it's been split off to
                // a linked audio lane; skip lanes that never carry media audio.
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
                }
                let Some(source) = clip.source_range() else {
                    continue;
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
                    source_frames: ticks_to_samples(
                        source.duration.value,
                        source.start.rate.num,
                        source.start.rate.den,
                    ),
                    retimed: clip.is_retimed(),
                    reversed: clip.reversed,
                    pitch_factor: clip.audio_pitch_factor(),
                    speed_curve: clip.has_speed_curve().then(|| clip.speed_curve.clone()),
                    // Rebase the envelope's clip-relative ticks into
                    // clip-relative output sample frames.
                    volume: clip
                        .volume
                        .map_ticks(|tick| ticks_to_samples(tick, fps.num, fps.den)),
                    fade_in: ticks_to_samples(clip.fade_in, fps.num, fps.den),
                    fade_out: ticks_to_samples(clip.fade_out, fps.num, fps.den),
                    denoise: clip.denoise,
                    reader: None,
                    rendered: None,
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
            })
        }
    }

    /// Mix every span overlapping `[pos, pos + out.len()/2)` into `out`
    /// (interleaved stereo; cleared to silence first).
    pub fn mix_into(&mut self, pos: i64, out: &mut [f32]) -> Result<(), EngineError> {
        out.fill(0.0);
        let block_frames = (out.len() / AUDIO_CHANNELS) as i64;
        let block_end = pos + block_frames;

        for span in &mut self.spans {
            if span.start >= block_end || span.end <= pos || span.exhausted {
                continue;
            }
            let s = span.start.max(pos);
            let e = span.end.min(block_end);

            // Rendered clips — retimed (M8 Phase 3) and/or denoised (M8 Phase
            // 5): mix from the buffer rendered fail-loud on first overlap and
            // served 1:1.
            if span.retimed || span.denoise {
                mix_rendered_span(span, pos, s, e, out)?;
                continue;
            }

            let reader = match &mut span.reader {
                Some(reader) => reader,
                None => {
                    let reader = AudioReader::open(&span.path, EXPORT_AUDIO_RATE)
                        .map_err(|err| audio_err("open audio source", &span.path, err))?;
                    span.reader.insert(reader)
                }
            };

            let src_from = span.source_start + (s - span.start);
            reader
                .seek_to_frame(src_from)
                .map_err(|err| audio_err("seek audio source", &span.path, err))?;
            // A stream that starts after the requested point leaves a lead
            // gap; shift the mix-in to keep the rest aligned (same handling
            // as the preview mixer).
            let lead = reader
                .position()
                .map_or(0, |p| (p - src_from).clamp(0, e - s));

            let want = ((e - s) - lead) as usize;
            if want == 0 {
                continue;
            }
            self.scratch.resize(want * AUDIO_CHANNELS, 0.0);
            let got = reader
                .read(&mut self.scratch[..want * AUDIO_CHANNELS])
                .map_err(|err| audio_err("decode audio source", &span.path, err))?;
            if got < want {
                span.exhausted = true;
            }

            let offset = ((s - pos + lead) as usize) * AUDIO_CHANNELS;
            let unity =
                span.volume.constant() == Some(1.0) && span.fade_in == 0 && span.fade_out == 0;
            if unity {
                for (dst, src) in out[offset..]
                    .iter_mut()
                    .zip(&self.scratch[..got * AUDIO_CHANNELS])
                {
                    *dst += *src;
                }
            } else {
                // Volume envelope + fade ramps (M1/M8): gain per sample frame
                // so automation and fades are smooth at sample resolution, not
                // block-stepped.
                let span_len = span.end - span.start;
                let first = s + lead - span.start;
                for frame in 0..got {
                    let gain = audio_gain_at(
                        first + frame as i64,
                        span_len,
                        &span.volume,
                        span.fade_in,
                        span.fade_out,
                    );
                    for ch in 0..AUDIO_CHANNELS {
                        out[offset + frame * AUDIO_CHANNELS + ch] +=
                            self.scratch[frame * AUDIO_CHANNELS + ch] * gain;
                    }
                }
            }
        }

        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
        Ok(())
    }
}

/// Render a span's full buffer (interleaved stereo) at the export rate. A
/// retime (M8 Phase 3) time-stretches — variable-rate for a ramp (M2), uniform
/// otherwise; a pure denoise (M8 Phase 5) reads the window 1:1. Denoise rides
/// on top of whichever, so the same inputs the preview mixer uses produce the
/// same buffer — what you hear is what you ship.
fn render_span(span: &Span) -> Result<Vec<f32>, EngineError> {
    let mut buf = if span.retimed {
        match &span.speed_curve {
            Some(curve) => cutlass_decoder::render_stretched_curve(
                &span.path,
                EXPORT_AUDIO_RATE,
                span.source_start,
                span.source_frames,
                span.end - span.start,
                span.reversed,
                span.pitch_factor,
                |p| cutlass_models::speed_curve_source_fraction(curve, p),
            ),
            None => cutlass_decoder::render_stretched(
                &span.path,
                EXPORT_AUDIO_RATE,
                span.source_start,
                span.source_frames,
                span.end - span.start,
                span.reversed,
                span.pitch_factor,
            ),
        }
        .map_err(|err| audio_err("render varispeed audio", &span.path, err))?
    } else {
        // Denoise-only: read the window 1:1 and clean it.
        cutlass_decoder::render_denoised(
            &span.path,
            EXPORT_AUDIO_RATE,
            span.source_start,
            span.end - span.start,
            span.end - span.start,
            span.reversed,
        )
        .map_err(|err| audio_err("render denoised audio", &span.path, err))?
    };
    // Denoise stacks on top of a retime's stretched buffer.
    if span.denoise && span.retimed {
        cutlass_decoder::denoise_interleaved(&mut buf);
    }
    Ok(buf)
}

/// Mix the overlap `[s, e)` of a rendered span — retimed (M8 Phase 3) and/or
/// denoised (M8 Phase 5) — from its buffer, rendering it fail-loud on first
/// overlap. Volume envelope and fades ride on top exactly like the 1× path.
fn mix_rendered_span(
    span: &mut Span,
    pos: i64,
    s: i64,
    e: i64,
    out: &mut [f32],
) -> Result<(), EngineError> {
    if span.rendered.is_none() {
        span.rendered = Some(render_span(span)?);
    }
    let buf = span.rendered.as_ref().expect("rendered above");
    let total_frames = (buf.len() / AUDIO_CHANNELS) as i64;
    let span_len = span.end - span.start;
    let unity = span.volume.constant() == Some(1.0) && span.fade_in == 0 && span.fade_out == 0;
    for f in s..e {
        let bi = f - span.start;
        if bi >= total_frames {
            break;
        }
        let dst = ((f - pos) as usize) * AUDIO_CHANNELS;
        let gain = if unity {
            1.0
        } else {
            audio_gain_at(bi, span_len, &span.volume, span.fade_in, span.fade_out)
        };
        let bi = bi as usize;
        for ch in 0..AUDIO_CHANNELS {
            out[dst + ch] += buf[bi * AUDIO_CHANNELS + ch] * gain;
        }
    }
    Ok(())
}

fn audio_err(what: &str, path: &std::path::Path, err: impl std::fmt::Display) -> EngineError {
    EngineError::Export(format!("{what} {}: {err}", path.display()))
}

/// `value` ticks at `num/den` fps → sample frames at the export rate
/// (exact i128, floored) — the same conversion the preview mixer uses.
fn ticks_to_samples(value: i64, num: i32, den: i32) -> i64 {
    if num <= 0 || den <= 0 {
        return 0;
    }
    let frames =
        i128::from(value) * i128::from(den) * i128::from(EXPORT_AUDIO_RATE) / i128::from(num);
    frames.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Sample-frame boundary of output video frame `n` at `out_num/out_den` fps:
/// the export loop pushes audio block `[boundary(n), boundary(n+1))` after
/// video frame `n`, so audio and video cover identical wall-clock spans.
pub fn sample_boundary(n: i64, out_num: i32, out_den: i32) -> i64 {
    ticks_to_samples(n, out_num, out_den)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutlass_models::{ClipId, MediaSource, Rational, RationalTime, TimeRange, TrackKind};

    #[test]
    fn ticks_to_samples_is_exact_for_common_rates() {
        // 24 ticks (1s at 24fps) = 48000 sample frames.
        assert_eq!(ticks_to_samples(24, 24, 1), 48_000);
        // One tick at 24fps = 2000 frames.
        assert_eq!(ticks_to_samples(1, 24, 1), 2_000);
        // NTSC: 30000 ticks at 30000/1001 = 1001s.
        assert_eq!(ticks_to_samples(30_000, 30_000, 1_001), 1_001 * 48_000);
    }

    #[test]
    fn sample_boundaries_partition_the_stream() {
        // 24fps: every video frame owns exactly 2000 samples.
        assert_eq!(sample_boundary(0, 24, 1), 0);
        assert_eq!(sample_boundary(1, 24, 1), 2_000);
        assert_eq!(sample_boundary(48, 24, 1), 96_000);
        // NTSC 30000/1001: boundaries are floored but cover every sample.
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
    fn video_lane_clip_with_audio_is_audible() {
        // CapCut keeps a video's sound on the clip itself: a media clip on a
        // *video* lane whose media carries audio contributes a span, with no
        // separate audio lane.
        let mut project = Project::new("test", Rational::FPS_24);
        let media = project.add_media(MediaSource::new(
            "/tmp/clip.mp4",
            640,
            480,
            Rational::FPS_24,
            100,
            true,
        ));
        let lane = project.add_track(TrackKind::Video, "V1");
        project
            .add_clip(
                lane,
                media,
                TimeRange::at_rate(0, 48, Rational::FPS_24),
                RationalTime::new(0, Rational::FPS_24),
            )
            .unwrap();
        let mixer = ExportAudioMixer::for_project(&project)
            .expect("a video clip with audio is audible from its own lane");
        assert_eq!(mixer.spans.len(), 1);
    }

    #[test]
    fn detached_video_half_defers_audio_to_its_linked_audio_lane() {
        // CapCut "separate audio": once a video clip's sound is split onto a
        // linked audio lane, the video half goes silent and only the audio-lane
        // partner sounds — no doubled audio.
        let mut project = Project::new("test", Rational::FPS_24);
        let media = project.add_media(MediaSource::new(
            "/tmp/clip.mp4",
            640,
            480,
            Rational::FPS_24,
            100,
            true,
        ));
        let v = project.add_track(TrackKind::Video, "V1");
        let a = project.add_track(TrackKind::Audio, "A1");
        let video_clip = project
            .add_clip(
                v,
                media,
                TimeRange::at_rate(0, 48, Rational::FPS_24),
                RationalTime::new(0, Rational::FPS_24),
            )
            .unwrap();
        let audio_clip = project
            .add_clip(
                a,
                media,
                TimeRange::at_rate(0, 48, Rational::FPS_24),
                RationalTime::new(0, Rational::FPS_24),
            )
            .unwrap();
        let link = cutlass_models::LinkId::next();
        for id in [video_clip, audio_clip] {
            project.timeline_mut().clip_mut(id).unwrap().link = Some(link);
        }
        let mixer =
            ExportAudioMixer::for_project(&project).expect("the audio-lane half still sounds");
        // Exactly one span — the audio-lane partner, not the now-silent video.
        assert_eq!(mixer.spans.len(), 1);
    }

    fn audio_clip_project(speed: Rational, reversed: bool) -> (Project, ClipId) {
        let mut project = Project::new("test", Rational::FPS_24);
        let media = project.add_media(MediaSource::new(
            "/tmp/clip.mp4",
            640,
            480,
            Rational::FPS_24,
            100,
            true,
        ));
        let lane = project.add_track(TrackKind::Audio, "A1");
        let clip = project
            .add_clip(
                lane,
                media,
                TimeRange::at_rate(0, 48, Rational::FPS_24),
                RationalTime::new(0, Rational::FPS_24),
            )
            .unwrap();
        project.set_clip_speed(clip, speed, reversed).unwrap();
        (project, clip)
    }

    #[test]
    fn constant_speed_clips_are_audible_and_carry_a_retimed_span() {
        // A 2× clip used to mute; now it time-stretches (M8 Phase 3).
        let (project, _clip) = audio_clip_project(Rational::new(2, 1), false);
        let mixer = ExportAudioMixer::for_project(&project).expect("2× clip is audible");
        assert_eq!(mixer.spans.len(), 1);
        let span = &mixer.spans[0];
        assert!(span.retimed, "constant speed change flags a retime");
        assert!(
            span.source_frames > 0,
            "the source window length is carried"
        );
        assert_eq!(span.pitch_factor, 1.0, "pitch is locked by default");

        // Reverse alone (1×) is also audible and retimed.
        let (project, _clip) = audio_clip_project(Rational::new(1, 1), true);
        let mixer = ExportAudioMixer::for_project(&project).expect("reversed clip is audible");
        assert!(mixer.spans[0].retimed && mixer.spans[0].reversed);
    }

    #[test]
    fn denoised_clip_carries_a_rendered_non_retimed_span() {
        // A 1× clip with noise reduction on is audible and serves from a
        // rendered buffer, but is not a retime (M8 Phase 5).
        let (mut project, clip) = audio_clip_project(Rational::new(1, 1), false);
        project.set_clip_denoise(clip, true).unwrap();
        let mixer = ExportAudioMixer::for_project(&project).expect("denoised clip is audible");
        assert_eq!(mixer.spans.len(), 1);
        let span = &mixer.spans[0];
        assert!(span.denoise, "the span carries the denoise flag");
        assert!(!span.retimed, "1× denoise is not a retime");
    }

    #[test]
    fn speed_curve_clips_are_audible_and_carry_the_ramp() {
        // A ramp used to mute on export; now it time-stretches variable-rate
        // (M8 Phase 3) so export matches the preview mixer.
        let (mut project, clip) = audio_clip_project(Rational::new(1, 1), false);
        let curve = Param::Keyframed {
            keyframes: vec![
                cutlass_models::Keyframe {
                    tick: 0,
                    value: 0.5,
                    easing: cutlass_models::Easing::Linear,
                },
                cutlass_models::Keyframe {
                    tick: cutlass_models::SPEED_CURVE_SCALE,
                    value: 2.0,
                    easing: cutlass_models::Easing::Linear,
                },
            ],
        };
        project.set_clip_speed_curve(clip, Some(curve)).unwrap();
        let mixer = ExportAudioMixer::for_project(&project).expect("a ramp is now audible");
        assert_eq!(mixer.spans.len(), 1);
        let span = &mixer.spans[0];
        assert!(span.retimed, "a ramp flags a retime");
        assert!(
            span.speed_curve.is_some(),
            "the ramp rides along to drive the variable-rate render"
        );
    }

    #[test]
    fn zero_volume_clips_are_skipped_and_fades_resolve_to_samples() {
        let mut project = Project::new("test", Rational::FPS_24);
        let media = project.add_media(MediaSource::new(
            "/tmp/clip.mp4",
            640,
            480,
            Rational::FPS_24,
            100,
            true,
        ));
        let lane = project.add_track(TrackKind::Audio, "A1");
        let clip = project
            .add_clip(
                lane,
                media,
                TimeRange::at_rate(0, 48, Rational::FPS_24),
                RationalTime::new(0, Rational::FPS_24),
            )
            .unwrap();

        // Muted by volume: no span at all.
        project
            .set_clip_audio(
                clip,
                Some(0.0),
                RationalTime::new(0, Rational::FPS_24),
                RationalTime::new(0, Rational::FPS_24),
            )
            .unwrap();
        assert!(
            ExportAudioMixer::for_project(&project).is_none(),
            "a muted clip contributes no audio"
        );

        // Audible with fades: span carries the gain shape in sample frames
        // (24 ticks at 24fps = 1s = 48000 sample frames).
        project
            .set_clip_audio(
                clip,
                Some(0.5),
                RationalTime::new(24, Rational::FPS_24),
                RationalTime::new(12, Rational::FPS_24),
            )
            .unwrap();
        let mixer = ExportAudioMixer::for_project(&project).expect("audible span");
        let span = &mixer.spans[0];
        assert_eq!(span.volume.constant(), Some(0.5));
        assert_eq!(span.fade_in, 48_000);
        assert_eq!(span.fade_out, 24_000);
    }

    #[test]
    fn muted_lanes_and_silent_media_are_skipped() {
        let mut project = Project::new("test", Rational::FPS_24);
        // Media without an audio stream on an audio lane: still silent.
        let silent = project.add_media(MediaSource::new(
            "/tmp/silent.mp4",
            640,
            480,
            Rational::FPS_24,
            100,
            false,
        ));
        let lane = project.add_track(TrackKind::Audio, "A1");
        project
            .add_clip(
                lane,
                silent,
                TimeRange::at_rate(0, 48, Rational::FPS_24),
                RationalTime::new(0, Rational::FPS_24),
            )
            .unwrap();
        assert!(ExportAudioMixer::for_project(&project).is_none());
    }
}
