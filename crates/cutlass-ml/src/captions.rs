//! Transcript → caption cues: group a word-timed [`Transcript`] into short,
//! readable lines the caller drops onto the timeline as text clips.
//!
//! Pure transcript post-processing — seconds in, seconds out, no timeline or
//! clip types — so the engine owns the seconds → tick mapping and this packing
//! logic unit-tests on synthetic transcripts. A caption *is* a text clip
//! starting at the cue's time (the M9 Phase 4 model: captions reuse the text
//! generator rather than a bespoke subtitle track), so all this layer decides
//! is **where the line breaks fall**: long runs of speech split into legible
//! cues instead of one wall of text.
//!
//! The packer prefers word timing when the backend reports it (whisper does),
//! breaking a line when it would grow too long, span too much time, follow a
//! long pause, or end a sentence. A backend without word timing degrades to one
//! cue per segment.

use crate::transcribe::Transcript;

/// One caption line: its text and time span in seconds from the start of the
/// transcribed audio (the same domain as [`crate::Word`]). The caller maps
/// `start`/`end` onto timeline ticks through the source clip's window.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptionCue {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// How aggressively to break the transcript into cues.
///
/// Defaults mirror common subtitle conventions: ~one readable line, a few
/// seconds on screen, and a fresh cue after a clear pause.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionLayout {
    /// Soft maximum characters per cue. A line breaks before a word that would
    /// push it past this (a single word longer than the limit still stands on
    /// its own).
    pub max_chars: usize,
    /// Maximum seconds a single cue stays on screen before forcing a break.
    pub max_duration: f64,
    /// A silence at least this long between consecutive words starts a new cue,
    /// so captions track natural phrase boundaries.
    pub max_gap: f64,
}

impl Default for CaptionLayout {
    fn default() -> Self {
        Self {
            max_chars: 42,
            max_duration: 6.0,
            max_gap: 0.8,
        }
    }
}

impl CaptionLayout {
    fn is_valid(&self) -> bool {
        self.max_chars > 0 && self.max_duration > 0.0 && self.max_gap >= 0.0
    }
}

/// True when `word` ends a sentence — a natural place to flush a cue.
fn ends_sentence(word: &str) -> bool {
    matches!(word.trim_end().chars().last(), Some('.' | '!' | '?' | '…'))
}

/// Pack a transcript into caption cues per `layout`. Segments with word timing
/// break on the character / duration / gap / sentence rules; segments without
/// word timing fall back to one cue each. Empty text is dropped, and every cue
/// has `end >= start`.
pub fn plan_captions(transcript: &Transcript, layout: &CaptionLayout) -> Vec<CaptionCue> {
    let layout = if layout.is_valid() {
        *layout
    } else {
        CaptionLayout::default()
    };

    let mut cues = Vec::new();
    for segment in &transcript.segments {
        if segment.words.is_empty() {
            push_cue(&mut cues, segment.text.trim(), segment.start, segment.end);
            continue;
        }
        pack_words(&mut cues, segment, &layout);
    }
    cues
}

/// Greedily group one segment's words into cues.
fn pack_words(cues: &mut Vec<CaptionCue>, segment: &crate::Segment, layout: &CaptionLayout) {
    let mut line = String::new();
    let mut start = 0.0;
    let mut prev_end = 0.0;

    for word in &segment.words {
        let token = word.text.trim();
        if token.is_empty() {
            continue;
        }

        if !line.is_empty() {
            let would_be = line.chars().count() + 1 + token.chars().count();
            let too_long = would_be > layout.max_chars;
            let too_late = word.end - start > layout.max_duration;
            let big_gap = word.start - prev_end >= layout.max_gap;
            if too_long || too_late || big_gap {
                push_cue(cues, &line, start, prev_end);
                line.clear();
            }
        }

        if line.is_empty() {
            start = word.start;
            line.push_str(token);
        } else {
            line.push(' ');
            line.push_str(token);
        }
        prev_end = word.end;

        if ends_sentence(token) {
            push_cue(cues, &line, start, prev_end);
            line.clear();
        }
    }

    push_cue(cues, &line, start, prev_end);
}

/// Append a cue if `text` is non-empty, clamping `end` to at least `start`.
fn push_cue(cues: &mut Vec<CaptionCue>, text: &str, start: f64, end: f64) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    cues.push(CaptionCue {
        text: text.to_string(),
        start,
        end: end.max(start),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::{Segment, Word};

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word {
            text: text.into(),
            start,
            end,
            confidence: None,
        }
    }

    fn segment(text: &str, start: f64, end: f64, words: Vec<Word>) -> Segment {
        Segment {
            text: text.into(),
            start,
            end,
            words,
        }
    }

    fn transcript(segments: Vec<Segment>) -> Transcript {
        Transcript {
            segments,
            language: Some("en".into()),
        }
    }

    #[test]
    fn short_phrase_becomes_one_cue() {
        let t = transcript(vec![segment(
            "hello world",
            0.0,
            1.0,
            vec![word("hello", 0.0, 0.5), word("world", 0.5, 1.0)],
        )]);
        let cues = plan_captions(&t, &CaptionLayout::default());
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "hello world");
        assert_eq!(cues[0].start, 0.0);
        assert_eq!(cues[0].end, 1.0);
    }

    #[test]
    fn breaks_a_long_line_on_the_character_budget() {
        // Six 5-char words with single spaces => "aaaaa ..." grows past a tiny
        // 12-char budget after the second word.
        let words: Vec<Word> = (0..6)
            .map(|i| word("aaaaa", i as f64, i as f64 + 0.5))
            .collect();
        let t = transcript(vec![segment("aaaaa…", 0.0, 6.0, words)]);
        let cues = plan_captions(
            &t,
            &CaptionLayout {
                max_chars: 12,
                ..CaptionLayout::default()
            },
        );
        // "aaaaa aaaaa" = 11 chars; a third word would be 17 > 12, so each cue
        // holds two words.
        assert_eq!(cues.len(), 3);
        assert!(cues.iter().all(|c| c.text == "aaaaa aaaaa"));
        assert_eq!(cues[0].start, 0.0);
        assert_eq!(cues[1].start, 2.0);
    }

    #[test]
    fn a_long_pause_starts_a_new_cue() {
        let t = transcript(vec![segment(
            "one two",
            0.0,
            5.0,
            vec![
                word("one", 0.0, 0.4),
                // 2s gap before "two" => new cue.
                word("two", 2.4, 2.8),
            ],
        )]);
        let cues = plan_captions(&t, &CaptionLayout::default());
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "one");
        assert_eq!(cues[1].text, "two");
        assert_eq!(cues[1].start, 2.4);
    }

    #[test]
    fn sentence_end_flushes_the_line() {
        let t = transcript(vec![segment(
            "Hi there. Go",
            0.0,
            3.0,
            vec![
                word("Hi", 0.0, 0.3),
                word("there.", 0.3, 0.8),
                word("Go", 0.9, 1.2),
            ],
        )]);
        let cues = plan_captions(&t, &CaptionLayout::default());
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Hi there.");
        assert_eq!(cues[0].end, 0.8);
        assert_eq!(cues[1].text, "Go");
    }

    #[test]
    fn duration_cap_forces_a_break() {
        let t = transcript(vec![segment(
            "tick tock",
            0.0,
            20.0,
            vec![word("tick", 0.0, 1.0), word("tock", 1.0, 12.0)],
        )]);
        let cues = plan_captions(
            &t,
            &CaptionLayout {
                max_duration: 6.0,
                ..CaptionLayout::default()
            },
        );
        // "tock" ends at 12s; 12 - 0 > 6 => it splits off.
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "tick");
        assert_eq!(cues[1].text, "tock");
    }

    #[test]
    fn segment_without_word_timing_is_one_cue() {
        let t = transcript(vec![segment("  whole line  ", 1.0, 4.0, vec![])]);
        let cues = plan_captions(&t, &CaptionLayout::default());
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "whole line");
        assert_eq!(cues[0].start, 1.0);
        assert_eq!(cues[0].end, 4.0);
    }

    #[test]
    fn blank_and_empty_inputs_drop_out() {
        assert!(plan_captions(&Transcript::default(), &CaptionLayout::default()).is_empty());
        let t = transcript(vec![segment("   ", 0.0, 1.0, vec![word("   ", 0.0, 1.0)])]);
        assert!(plan_captions(&t, &CaptionLayout::default()).is_empty());
    }

    #[test]
    fn invalid_layout_falls_back_to_defaults() {
        let t = transcript(vec![segment(
            "hello world",
            0.0,
            1.0,
            vec![word("hello", 0.0, 0.5), word("world", 0.5, 1.0)],
        )]);
        let cues = plan_captions(
            &t,
            &CaptionLayout {
                max_chars: 0,
                max_duration: 0.0,
                max_gap: -1.0,
            },
        );
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "hello world");
    }
}
