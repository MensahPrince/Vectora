//! Auto-captions (AI media roadmap M9 Phase 4): "caption this clip" → decode
//! the clip's speech, transcribe it ([`cutlass_ml::Transcribe`]), and lay the
//! words down as ordinary subtitle-styled text clips on a captions lane.
//!
//! A caption is just a text clip that starts at the cue's time — so this owns
//! no bespoke subtitle model: it reuses [`Generator::Text`] and the existing
//! add-clip primitives, and the resulting cues drag, trim, restyle, and delete
//! like any title. Like the M9 silence pass and the M8 DSP hooks the heavy
//! work (transcription) lives behind a seam (`cutlass-ml`, runtime-injected)
//! and the decode is shared ([`crate::clip_audio`]), while this module owns the
//! seconds → tick mapping and the subtitle look. The pure parts
//! ([`build_placements`], [`plan_caption_ranges`]) split from the decode so they
//! unit-test without media or a model.
//!
//! Deliberate gaps (tracked in `docs/ai-media-roadmap.md`): retimed clips are
//! rejected (the seconds → tick mapping is linear only at 1×), captions land on
//! a fresh lane each run (no "append to the existing captions track" yet), and
//! the cues carry no caption-group tag (M7's styled subtitle lane / SRT export
//! ride a follow-up — for now they are indistinguishable from hand-made text).

use std::sync::atomic::AtomicBool;

use cutlass_ml::{CaptionLayout, Transcribe, TranscribeOptions, Transcript, plan_captions};
use cutlass_models::{
    ClipId, Generator, ModelError, Project, Rational, TextAlignV, TextStroke, TextStyle, TimeRange,
};

use crate::clip_audio::{self, ANALYSIS_RATE};
use crate::error::EngineError;

/// Caption text size in reference pixels (1080px canvas) — smaller than the
/// 90px title default so a full subtitle line fits the lower third.
const CAPTION_SIZE: f32 = 64.0;

/// Subtitle look: bottom-anchored, centered (the [`TextStyle`] default), white
/// fill with a black outline so it stays legible over any footage.
fn subtitle_style() -> TextStyle {
    TextStyle {
        size: CAPTION_SIZE,
        align_v: TextAlignV::Bottom,
        stroke: Some(TextStroke {
            rgba: [0, 0, 0, 255],
            width: 8.0,
        }),
        ..TextStyle::default()
    }
}

/// Decode a clip's speech, transcribe it, and turn the result into subtitle
/// text-clip placements (`generator`, timeline range) ready to add. Rejects
/// generated clips, media without audio, and retimed clips. Returns an empty
/// vec when the backend heard no speech in the clip's window.
pub(crate) fn plan(
    project: &Project,
    clip: ClipId,
    transcriber: &dyn Transcribe,
    options: &TranscribeOptions,
    layout: &CaptionLayout,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(f32),
) -> Result<Vec<(Generator, TimeRange)>, EngineError> {
    let target = project.clip(clip).ok_or(ModelError::UnknownClip(clip))?;
    if target.is_retimed() {
        return Err(ModelError::InvalidParam(
            "captions do not yet support retimed clips".into(),
        )
        .into());
    }
    let span = target.timeline;
    let fps = project.timeline().frame_rate;

    let mono = clip_audio::decode_clip_mono(project, clip)?;
    let transcript = transcriber.transcribe(&mono, ANALYSIS_RATE, options, cancel, on_progress)?;

    Ok(build_placements(
        &transcript,
        span.start.value,
        span.end_tick(),
        fps,
        layout,
    ))
}

/// Pure: transcript → subtitle text-clip placements within the clip's span.
/// Packs cues ([`plan_captions`]), maps each to a timeline range, and wraps the
/// text in a [`Generator::Text`] with the subtitle style.
pub(crate) fn build_placements(
    transcript: &Transcript,
    clip_start: i64,
    clip_end: i64,
    fps: Rational,
    layout: &CaptionLayout,
) -> Vec<(Generator, TimeRange)> {
    let cues = plan_captions(transcript, layout);
    plan_caption_ranges(clip_start, clip_end, fps, &cues)
        .into_iter()
        .map(|(text, range)| {
            (
                Generator::Text {
                    content: text,
                    style: subtitle_style(),
                },
                range,
            )
        })
        .collect()
}

/// Map caption cues (seconds from the clip's window start) to absolute timeline
/// tick ranges, anchored at the clip's start. Clamps each cue to the clip's
/// `[start, end)`, drops empties, and trims each cue's end to the next cue's
/// start so the placements never overlap (the timeline rejects overlaps on a
/// lane). Pure — the linear seconds → tick mapping holds because retimed clips
/// are rejected upstream.
fn plan_caption_ranges(
    clip_start: i64,
    clip_end: i64,
    fps: Rational,
    cues: &[cutlass_ml::CaptionCue],
) -> Vec<(String, TimeRange)> {
    if fps.num <= 0 || fps.den <= 0 || clip_end <= clip_start {
        return Vec::new();
    }
    let fps_f = f64::from(fps.num) / f64::from(fps.den);

    // First pass: map to clamped tick ranges, dropping empties.
    let mut raw: Vec<(String, i64, i64)> = Vec::new();
    for cue in cues {
        let text = cue.text.trim();
        if text.is_empty() {
            continue;
        }
        let a = ((clip_start as f64 + cue.start * fps_f).round() as i64).clamp(clip_start, clip_end);
        let b = ((clip_start as f64 + cue.end * fps_f).round() as i64).clamp(clip_start, clip_end);
        if b <= a {
            continue;
        }
        raw.push((text.to_string(), a, b));
    }

    // Second pass: clamp each end to the following cue's start so abutting cues
    // stay disjoint after rounding.
    let mut out = Vec::with_capacity(raw.len());
    for i in 0..raw.len() {
        let next_start = raw.get(i + 1).map(|n| n.1);
        let (text, a, b) = &raw[i];
        let end = next_start.map_or(*b, |n| (*b).min(n));
        if end <= *a {
            continue;
        }
        out.push((text.clone(), TimeRange::at_rate(*a, end - *a, fps)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutlass_ml::{CaptionCue, Segment, Word};

    const R24: Rational = Rational::FPS_24;

    fn cue(text: &str, start: f64, end: f64) -> CaptionCue {
        CaptionCue {
            text: text.into(),
            start,
            end,
        }
    }

    // --- plan_caption_ranges ----------------------------------------------

    #[test]
    fn maps_cue_seconds_to_ticks_anchored_at_the_clip() {
        // Clip [24,72) at 24 fps. A cue at [0.5,1.0) s → ticks [36,48).
        let ranges = plan_caption_ranges(24, 72, R24, &[cue("hi", 0.5, 1.0)]);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].0, "hi");
        assert_eq!(ranges[0].1, TimeRange::at_rate(36, 12, R24));
    }

    #[test]
    fn clamps_to_the_clip_and_drops_out_of_range() {
        // A cue running past the clip end clamps; one entirely past is dropped.
        let ranges = plan_caption_ranges(0, 48, R24, &[cue("a", 1.5, 9.0), cue("b", 10.0, 11.0)]);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].0, "a");
        assert_eq!(ranges[0].1, TimeRange::at_rate(36, 12, R24));
    }

    #[test]
    fn trims_overlap_so_placements_stay_disjoint() {
        // Two cues whose rounded ticks would overlap: the first is trimmed to
        // end where the second begins.
        let ranges = plan_caption_ranges(0, 96, R24, &[cue("one", 0.0, 1.2), cue("two", 1.0, 2.0)]);
        assert_eq!(ranges.len(), 2);
        let (_, first) = &ranges[0];
        let (_, second) = &ranges[1];
        assert_eq!(first.end_tick(), second.start.value);
        assert!(first.duration.value > 0);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(plan_caption_ranges(0, 0, R24, &[cue("x", 0.0, 1.0)]).is_empty());
        assert!(plan_caption_ranges(0, 48, R24, &[cue("  ", 0.0, 1.0)]).is_empty());
        assert!(plan_caption_ranges(0, 48, Rational::new(0, 1), &[cue("x", 0.0, 1.0)]).is_empty());
    }

    // --- build_placements --------------------------------------------------

    #[test]
    fn builds_subtitle_styled_text_clips() {
        let transcript = Transcript {
            segments: vec![Segment {
                text: "hello world".into(),
                start: 0.0,
                end: 1.0,
                words: vec![
                    Word {
                        text: "hello".into(),
                        start: 0.0,
                        end: 0.5,
                        confidence: None,
                    },
                    Word {
                        text: "world".into(),
                        start: 0.5,
                        end: 1.0,
                        confidence: None,
                    },
                ],
            }],
            language: Some("en".into()),
        };
        let placements = build_placements(&transcript, 0, 48, R24, &CaptionLayout::default());
        assert_eq!(placements.len(), 1);
        let (generator, range) = &placements[0];
        match generator {
            Generator::Text { content, style } => {
                assert_eq!(content, "hello world");
                assert_eq!(style.align_v, TextAlignV::Bottom);
                assert!(style.stroke.is_some(), "captions get a legibility outline");
            }
            other => panic!("expected a text generator, got {other:?}"),
        }
        assert_eq!(*range, TimeRange::at_rate(0, 24, R24));
    }

    #[test]
    fn silent_transcript_yields_no_placements() {
        assert!(
            build_placements(&Transcript::default(), 0, 48, R24, &CaptionLayout::default())
                .is_empty()
        );
    }
}
