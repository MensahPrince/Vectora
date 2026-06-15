//! Silence detection for AutoCut (AI media roadmap M9 Phase 1).
//!
//! The pure DSP behind "cut the silences out of this": turn a clip's mono
//! samples into a list of silent spans worth removing. The pipeline is the
//! simple, robust one — a control-rate **broadband RMS envelope**, a
//! **threshold** that marks quiet hops, and a **minimum-duration** filter so
//! the breaths and beats between words survive while real pauses are flagged.
//! Each surviving span is shrunk inward by a small **keep margin** so a cut
//! never clips a word's onset or tail.
//!
//! Everything here is pure and sample-domain: it takes mono `f32` and returns
//! plain `Vec`s with no media, model, or timeline types — the same model-free
//! seam the beat detection and ducking analysis use, so the tricky parts
//! unit-test on synthetic input. The engine owns the decode (via
//! [`AudioReader`](crate::AudioReader)) and the mapping of silence seconds
//! into clip-relative ticks and cut ranges.

/// Control rate of the RMS envelope, in hertz: one analysis step every 10 ms,
/// matching the ducking sidechain. Pauses in speech are far longer than this,
/// so 100 Hz localizes a silence edge to a hop's worth of time while keeping
/// the scan cheap.
const CONTROL_HZ: f32 = 100.0;

/// AutoCut silence-detection settings. Linear units for the level (the
/// command/UI layer converts a dB threshold if it wants to present one),
/// seconds for the durations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SilenceSettings {
    /// Broadband RMS (linear, ~`0..1`) at or below which a hop counts as
    /// silent.
    pub threshold: f32,
    /// Shortest silence worth cutting, in seconds. Gaps shorter than this —
    /// the breaths and beats between words — are kept, so speech stays
    /// natural.
    pub min_silence: f32,
    /// Audio kept on each side of a detected silence, in seconds, so a cut
    /// never clips a word's onset or tail. Each span is shrunk inward by this
    /// on both ends.
    pub keep_padding: f32,
}

impl Default for SilenceSettings {
    fn default() -> Self {
        // -40 dBFS ≈ 0.01 linear: below conversational speech, above a quiet
        // room tone. Cut pauses of half a second or more, keeping ~80 ms of
        // air around each kept phrase — broadcast-typical AutoCut defaults the
        // UI/agent override.
        Self {
            threshold: 0.01,
            min_silence: 0.5,
            keep_padding: 0.08,
        }
    }
}

/// Samples per control step at `sample_rate` (at least one).
fn hop_samples(sample_rate: u32) -> usize {
    ((sample_rate as f32 / CONTROL_HZ).round() as usize).max(1)
}

/// Seconds covered by one control-rate hop at `sample_rate` (`0` for a zero
/// rate). The realized hop is a whole number of samples, so it differs
/// slightly from `1/CONTROL_HZ`; report what the data actually is.
fn hop_seconds(sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        return 0.0;
    }
    hop_samples(sample_rate) as f64 / f64::from(sample_rate)
}

/// Reduce mono samples to a control-rate broadband RMS envelope: the RMS of
/// each `1/CONTROL_HZ`-second hop. One value per hop, including a final short
/// hop. Empty / zero-rate input yields an empty envelope.
pub fn rms_envelope(mono: &[f32], sample_rate: u32) -> Vec<f32> {
    if mono.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let hop = hop_samples(sample_rate);
    let mut env = Vec::with_capacity(mono.len() / hop + 1);
    let mut sum_sq = 0.0f64;
    let mut filled = 0usize;
    for &s in mono {
        sum_sq += f64::from(s) * f64::from(s);
        filled += 1;
        if filled == hop {
            env.push((sum_sq / hop as f64).sqrt() as f32);
            sum_sq = 0.0;
            filled = 0;
        }
    }
    if filled > 0 {
        env.push((sum_sq / filled as f64).sqrt() as f32);
    }
    env
}

/// Detect silent spans (seconds from the start of `mono`) worth cutting.
///
/// Builds a control-rate RMS envelope, marks every hop at or below
/// `threshold` as silent, coalesces contiguous silent hops into runs, keeps
/// only runs at least `min_silence` long, then shrinks each surviving run
/// inward by `keep_padding` on both sides (dropping any that collapse). The
/// result is a list of non-overlapping `(start, end)` spans in increasing
/// order, each strictly positive in length. Audio that is entirely above the
/// threshold returns no spans; audio that is entirely silent returns one span.
pub fn detect_silences(
    mono: &[f32],
    sample_rate: u32,
    settings: &SilenceSettings,
) -> Vec<(f64, f64)> {
    let env = rms_envelope(mono, sample_rate);
    silences_from_envelope(&env, hop_seconds(sample_rate), settings)
}

/// Find silent spans over a precomputed envelope where each value covers
/// `hop_seconds` of audio (split out so the run / keep-margin logic unit-tests
/// without an RMS pass).
fn silences_from_envelope(
    env: &[f32],
    hop_seconds: f64,
    settings: &SilenceSettings,
) -> Vec<(f64, f64)> {
    if env.is_empty() || hop_seconds <= 0.0 {
        return Vec::new();
    }
    let min_silence = f64::from(settings.min_silence.max(0.0));
    let pad = f64::from(settings.keep_padding.max(0.0));

    let mut spans = Vec::new();
    let mut run_start: Option<usize> = None;
    // One past the end so a run that reaches the final hop is closed too.
    for i in 0..=env.len() {
        let silent = i < env.len() && env[i] <= settings.threshold;
        match (silent, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(start)) => {
                let start_s = start as f64 * hop_seconds;
                let end_s = i as f64 * hop_seconds;
                if end_s - start_s >= min_silence {
                    let (a, b) = (start_s + pad, end_s - pad);
                    if b > a {
                        spans.push((a, b));
                    }
                }
                run_start = None;
            }
            _ => {}
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Build a signal from `(seconds, amplitude)` segments. A zero amplitude
    /// is silence; otherwise a 440 Hz sine at that amplitude.
    fn segments(rate: u32, pattern: &[(f32, f32)]) -> Vec<f32> {
        let mut buf = Vec::new();
        for &(secs, amp) in pattern {
            let n = (rate as f32 * secs) as usize;
            for i in 0..n {
                buf.push(if amp == 0.0 {
                    0.0
                } else {
                    amp * (2.0 * PI * 440.0 * i as f32 / rate as f32).sin()
                });
            }
        }
        buf
    }

    // --- rms_envelope ------------------------------------------------------

    #[test]
    fn empty_or_zero_rate_is_empty() {
        assert!(rms_envelope(&[], 16_000).is_empty());
        assert!(rms_envelope(&[0.1, 0.2], 0).is_empty());
        assert!(detect_silences(&[], 16_000, &SilenceSettings::default()).is_empty());
    }

    #[test]
    fn silence_reads_as_no_energy() {
        let env = rms_envelope(&vec![0.0; 16_000], 16_000);
        assert!(!env.is_empty());
        assert!(env.iter().all(|&e| e < 1e-6), "silence has no energy");
    }

    #[test]
    fn envelope_length_tracks_control_rate() {
        // 1 s at 100 Hz control rate → ~100 hops.
        let env = rms_envelope(&segments(16_000, &[(1.0, 0.5)]), 16_000);
        assert!((env.len() as i32 - 100).abs() <= 1, "got {}", env.len());
    }

    // --- detect_silences ---------------------------------------------------

    #[test]
    fn finds_a_long_pause_and_keeps_the_margin() {
        let rate = 16_000;
        // Loud / silent / loud, one second each. The middle second is the only
        // silence and it clears the half-second minimum.
        let sig = segments(rate, &[(1.0, 0.5), (1.0, 0.0), (1.0, 0.5)]);
        let spans = detect_silences(&sig, rate, &SilenceSettings::default());
        assert_eq!(spans.len(), 1, "one pause, got {spans:?}");
        let (a, b) = spans[0];
        // [1.0, 2.0] shrunk inward by the 80 ms keep margin.
        assert!((a - 1.08).abs() < 0.02, "start {a}");
        assert!((b - 1.92).abs() < 0.02, "end {b}");
    }

    #[test]
    fn short_gaps_are_kept() {
        let rate = 16_000;
        // A 0.3 s gap is shorter than the half-second minimum — not a cut.
        let sig = segments(rate, &[(1.0, 0.5), (0.3, 0.0), (1.0, 0.5)]);
        let spans = detect_silences(&sig, rate, &SilenceSettings::default());
        assert!(spans.is_empty(), "kept the breath, got {spans:?}");
    }

    #[test]
    fn loud_throughout_has_no_silences() {
        let rate = 16_000;
        let sig = segments(rate, &[(2.0, 0.5)]);
        assert!(detect_silences(&sig, rate, &SilenceSettings::default()).is_empty());
    }

    #[test]
    fn all_silence_is_one_span() {
        let rate = 16_000;
        let spans = detect_silences(
            &vec![0.0; rate as usize * 2],
            rate,
            &SilenceSettings::default(),
        );
        assert_eq!(spans.len(), 1);
        let (a, b) = spans[0];
        assert!(
            a > 0.0 && b < 2.0 && b > a,
            "padded whole-clip span: {a}..{b}"
        );
    }

    #[test]
    fn spans_are_ordered_and_non_overlapping() {
        let rate = 16_000;
        let sig = segments(
            rate,
            &[(0.6, 0.5), (0.8, 0.0), (0.6, 0.5), (0.7, 0.0), (0.6, 0.5)],
        );
        let spans = detect_silences(&sig, rate, &SilenceSettings::default());
        assert_eq!(spans.len(), 2, "two pauses, got {spans:?}");
        assert!(spans[0].1 <= spans[1].0, "ordered, disjoint: {spans:?}");
        assert!(spans.iter().all(|&(a, b)| b > a));
    }

    // --- silences_from_envelope (pure run logic) ---------------------------

    #[test]
    fn min_silence_filters_by_duration() {
        // Three silent hops (0.03 s) below a 0.05 s minimum → nothing.
        let env = [1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0];
        let s = SilenceSettings {
            threshold: 0.5,
            min_silence: 0.05,
            keep_padding: 0.0,
        };
        assert!(silences_from_envelope(&env, 0.01, &s).is_empty());
    }

    #[test]
    fn keep_padding_drops_spans_that_collapse() {
        // A 0.05 s silence with a 0.03 s margin each side leaves nothing.
        let env = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let s = SilenceSettings {
            threshold: 0.5,
            min_silence: 0.04,
            keep_padding: 0.03,
        };
        assert!(silences_from_envelope(&env, 0.01, &s).is_empty());
    }

    #[test]
    fn trailing_silence_run_is_closed() {
        // A run reaching the final hop must still emit.
        let env = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let s = SilenceSettings {
            threshold: 0.5,
            min_silence: 0.04,
            keep_padding: 0.0,
        };
        let spans = silences_from_envelope(&env, 0.01, &s);
        assert_eq!(spans.len(), 1);
        assert!((spans[0].0 - 0.02).abs() < 1e-9, "{spans:?}");
        assert!((spans[0].1 - 0.07).abs() < 1e-9, "{spans:?}");
    }
}
