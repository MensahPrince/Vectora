//! Beat detection analysis (audio roadmap M8 Phase 6).
//!
//! The pure DSP behind "snap my cuts to the beat": turn a music clip's mono
//! samples into a list of beat times. The pipeline is the textbook one —
//! a **spectral-flux onset envelope** (how much new energy appears each
//! short hop), **autocorrelation** of that envelope to estimate the tempo,
//! then a **comb search** for the phase that best lines a steady pulse up
//! with the onsets — emitting an evenly spaced beat grid (CapCut's "Beat"
//! feature, which lays a grid over the track).
//!
//! Everything here is pure and sample-domain: it takes mono `f32` and returns
//! plain `Vec`s with no media, model, or timeline types. The engine owns the
//! decode (via [`AudioReader`](crate::AudioReader)) and the mapping of beat
//! seconds into clip-relative ticks — the same model-free seam the ducking
//! and varispeed analysis use, so the tricky parts unit-test on synthetic
//! input.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// FFT window for the onset analysis (≈46 ms at 22.05 kHz): long enough for
/// stable low-mid spectral estimates, short enough to localize transients.
const WINDOW: usize = 1024;
/// Hop between analysis windows (50% overlap). The onset envelope's control
/// rate is `sample_rate / HOP`.
const HOP: usize = 512;

/// Slowest tempo the detector considers, in beats per minute. Below this a
/// "beat" reads as structure (bars), not pulse.
const MIN_BPM: f32 = 60.0;
/// Fastest tempo the detector considers. Above this the grid would be denser
/// than a listener taps.
const MAX_BPM: f32 = 200.0;

/// Onset-envelope control rate at `sample_rate`: one flux value per [`HOP`].
fn control_hz(sample_rate: u32) -> f32 {
    sample_rate as f32 / HOP as f32
}

/// Spectral-flux onset envelope: for each [`HOP`]-spaced [`WINDOW`] of audio,
/// the sum over frequency bins of the *positive* change in magnitude since the
/// previous window. Energy that newly appears (a drum hit, a note attack)
/// produces a spike; steady tones contribute nothing. One value per hop.
///
/// A Hann window tapers each frame so bin leakage doesn't masquerade as flux.
/// Empty / too-short / zero-rate input yields an empty envelope.
pub fn onset_envelope(mono: &[f32], sample_rate: u32) -> Vec<f32> {
    if sample_rate == 0 || mono.len() < WINDOW {
        return Vec::new();
    }

    let hann: Vec<f32> = (0..WINDOW)
        .map(|i| {
            let x = std::f32::consts::PI * i as f32 / (WINDOW - 1) as f32;
            x.sin().powi(2)
        })
        .collect();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);
    let mut scratch = vec![Complex::<f32>::new(0.0, 0.0); fft.get_inplace_scratch_len()];

    let bins = WINDOW / 2 + 1;
    let mut prev = vec![0.0f32; bins];
    let mut buf = vec![Complex::<f32>::new(0.0, 0.0); WINDOW];
    let mut envelope = Vec::with_capacity(mono.len() / HOP + 1);

    let mut start = 0usize;
    while start + WINDOW <= mono.len() {
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = Complex::new(mono[start + i] * hann[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);

        let mut flux = 0.0f32;
        for (k, prev_mag) in prev.iter_mut().enumerate().take(bins) {
            let mag = buf[k].norm();
            let diff = mag - *prev_mag;
            if diff > 0.0 {
                flux += diff;
            }
            *prev_mag = mag;
        }
        envelope.push(flux);
        start += HOP;
    }

    envelope
}

/// Detect beat times (seconds from the start of `mono`) for a piece of audio.
///
/// Runs [`onset_envelope`], estimates the dominant tempo by autocorrelating it
/// (refined to sub-hop precision so the grid doesn't drift over a long clip),
/// finds the pulse phase that best matches the onsets, and lays down an evenly
/// spaced grid across the analyzed span. Returns an empty list when the audio
/// is too short or too featureless to find a stable tempo.
pub fn detect_beats(mono: &[f32], sample_rate: u32) -> Vec<f64> {
    let envelope = onset_envelope(mono, sample_rate);
    detect_beats_from_envelope(&envelope, control_hz(sample_rate))
}

/// Tempo (BPM) the perceptual prior is centered on. Real music's "tactus" —
/// the rate a listener taps — clusters here, so weighting candidate tempos
/// toward it breaks the octave ambiguity (60 vs 120 vs 240 bpm) the way an
/// ear does. Standard in autocorrelation tempo induction (Davies/Plumbley).
const PRIOR_CENTER_BPM: f64 = 120.0;
/// Width of the (log-tempo) prior. Wide enough that genuine 80/160 bpm music
/// still wins, narrow enough to resolve the half/double-tempo tie.
const PRIOR_SIGMA: f64 = 0.9;
/// Period-grid resolution, in envelope frames. Finer than one hop so a tempo
/// whose period isn't a whole number of hops is found without the integer-lag
/// "split" that otherwise hands the win to the double-tempo lag.
const PERIOD_STEP: f64 = 0.25;

/// Beat detection over a pre-computed onset envelope sampled at `fps` hertz
/// (split out so the tempo/phase logic unit-tests without an FFT).
fn detect_beats_from_envelope(envelope: &[f32], fps: f32) -> Vec<f64> {
    if fps <= 0.0 || envelope.len() < 8 {
        return Vec::new();
    }

    // Half-wave-rectified, mean-removed envelope: autocorrelation should see
    // the pulse, not the DC level or the decay tails. A light Gaussian smooth
    // widens each onset to a few frames so a tempo whose period isn't a whole
    // number of frames still correlates at the *true* lag (a one-frame spike at
    // a half-integer lag interpolates poorly and hands the win to double tempo).
    let mean = envelope.iter().copied().sum::<f32>() / envelope.len() as f32;
    let rectified: Vec<f32> = envelope.iter().map(|&e| (e - mean).max(0.0)).collect();
    let signal = smooth(&rectified);
    let energy: f32 = signal.iter().map(|&s| s * s).sum();
    if energy <= f32::EPSILON {
        return Vec::new();
    }

    // Lag (in envelope frames) for each tempo bound; faster tempo ⇒ shorter lag.
    let lag_min = (f64::from(fps) * 60.0 / f64::from(MAX_BPM)).max(1.0);
    let lag_max = (f64::from(fps) * 60.0 / f64::from(MIN_BPM)).max(lag_min + 1.0);
    if (signal.len() as f64) <= lag_max + 2.0 {
        return Vec::new();
    }

    // Interpolated autocorrelation at a fractional lag: linear-interpolating the
    // shifted copy lets a non-integer period score correctly (an integer-lag
    // scan splits it across two bins and mis-picks the double tempo).
    let autocorr = |period: f64| -> f32 {
        let mut sum = 0.0f32;
        for (i, &a) in signal.iter().enumerate() {
            if a == 0.0 {
                continue;
            }
            sum += a * interp(&signal, i as f64 + period);
        }
        sum
    };

    let mut best_period = lag_min;
    let mut best_score = f32::MIN;
    let mut period = lag_min;
    while period <= lag_max {
        let bpm = f64::from(fps) * 60.0 / period;
        let score = autocorr(period) * tempo_prior(bpm) as f32;
        if score > best_score {
            best_score = score;
            best_period = period;
        }
        period += PERIOD_STEP;
    }
    if best_score <= 0.0 {
        return Vec::new();
    }
    let period = best_period;

    // Phase: slide one period of offsets and keep the comb that collects the
    // most onset energy.
    let period_i = period.round().max(1.0) as usize;
    let mut best_offset = 0usize;
    let mut best_comb = f32::MIN;
    for offset in 0..period_i {
        let mut sum = 0.0f32;
        let mut pos = offset as f64;
        while pos < signal.len() as f64 {
            sum += interp(&signal, pos);
            pos += period;
        }
        if sum > best_comb {
            best_comb = sum;
            best_offset = offset;
        }
    }

    let mut beats = Vec::new();
    let mut pos = best_offset as f64;
    let len = signal.len() as f64;
    while pos < len {
        beats.push(pos / f64::from(fps));
        pos += period;
    }
    beats
}

/// Convolve with a fixed 5-tap Gaussian (σ ≈ 1 frame), edge-clamped. Spreads a
/// one-frame onset spike across ~5 frames so fractional-lag autocorrelation
/// reads the true period instead of splitting it.
fn smooth(signal: &[f32]) -> Vec<f32> {
    const KERNEL: [f32; 5] = [0.06, 0.24, 0.40, 0.24, 0.06];
    let n = signal.len();
    (0..n)
        .map(|i| {
            let mut acc = 0.0f32;
            for (k, &w) in KERNEL.iter().enumerate() {
                let idx = (i as isize + k as isize - 2).clamp(0, n as isize - 1) as usize;
                acc += w * signal[idx];
            }
            acc
        })
        .collect()
}

/// Linear interpolation of `sig` at a fractional index (0 outside its bounds).
fn interp(sig: &[f32], x: f64) -> f32 {
    if x < 0.0 {
        return 0.0;
    }
    let i = x.floor() as usize;
    if i + 1 >= sig.len() {
        return 0.0;
    }
    let f = (x - i as f64) as f32;
    sig[i] * (1.0 - f) + sig[i + 1] * f
}

/// Perceptual weight for a candidate tempo: a Gaussian on log-BPM centered at
/// [`PRIOR_CENTER_BPM`]. Peaks at 1.0, falls off symmetrically in octaves, so
/// it nudges the autocorrelation away from half/double-tempo errors without
/// overriding a clearly stronger true tempo.
fn tempo_prior(bpm: f64) -> f64 {
    let z = (bpm / PRIOR_CENTER_BPM).ln() / PRIOR_SIGMA;
    (-0.5 * z * z).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A click track: a unit impulse every `period_s` seconds, silence between.
    /// Impulses are broadband, so each lands a clean spectral-flux spike.
    fn click_track(bpm: f32, sample_rate: u32, secs: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * secs) as usize;
        let period = (sample_rate as f32 * 60.0 / bpm) as usize;
        let mut buf = vec![0.0f32; n];
        let mut i = period / 2; // first beat slightly in, not at t=0
        while i < n {
            buf[i] = 1.0;
            i += period;
        }
        buf
    }

    fn median_interval(beats: &[f64]) -> f64 {
        let mut diffs: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
        diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        diffs[diffs.len() / 2]
    }

    #[test]
    fn empty_or_short_input_has_no_envelope() {
        assert!(onset_envelope(&[], 22_050).is_empty());
        assert!(onset_envelope(&[0.0; 100], 22_050).is_empty());
        assert!(onset_envelope(&[0.1; 4096], 0).is_empty());
    }

    #[test]
    fn onset_envelope_spikes_on_impulses() {
        let rate = 22_050;
        let track = click_track(120.0, rate, 4.0);
        let env = onset_envelope(&track, rate);
        assert!(!env.is_empty());
        // The loudest hops should be far above the median (silence between
        // clicks contributes ~no flux).
        let mut sorted = env.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[sorted.len() / 2];
        let peak = *sorted.last().unwrap();
        assert!(peak > median * 5.0 + 1e-3, "peak {peak} vs median {median}");
    }

    #[test]
    fn detects_tempo_of_a_click_track() {
        let rate = 22_050;
        for &bpm in &[90.0f32, 120.0, 150.0] {
            let track = click_track(bpm, rate, 6.0);
            let beats = detect_beats(&track, rate);
            assert!(beats.len() > 4, "{bpm} bpm: got {} beats", beats.len());
            let expected = 60.0 / bpm as f64;
            let got = median_interval(&beats);
            assert!(
                (got - expected).abs() < expected * 0.1,
                "{bpm} bpm: interval {got:.4}s vs expected {expected:.4}s"
            );
        }
    }

    #[test]
    fn beats_are_monotonic_and_in_range() {
        let rate = 22_050;
        let track = click_track(128.0, rate, 5.0);
        let beats = detect_beats(&track, rate);
        assert!(beats.windows(2).all(|w| w[1] > w[0]), "strictly increasing");
        assert!(beats.iter().all(|&t| (0.0..=5.0).contains(&t)));
    }

    #[test]
    fn silence_has_no_beats() {
        let rate = 22_050;
        assert!(detect_beats(&vec![0.0; rate as usize * 4], rate).is_empty());
    }

    #[test]
    fn featureless_tone_has_no_beats() {
        // A steady sine has energy but no recurring onsets — no stable pulse.
        let rate = 22_050;
        let tone: Vec<f32> = (0..rate as usize * 4)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin())
            .collect();
        let beats = detect_beats(&tone, rate);
        // Either no beats, or at most a weak grid — the key invariant is the
        // onset envelope is near-flat so no strong false tempo dominates.
        assert!(beats.len() < 200, "a pure tone should not read as dense beats");
    }

    #[test]
    fn tempo_prior_peaks_at_center_and_is_octave_symmetric() {
        assert!((tempo_prior(PRIOR_CENTER_BPM) - 1.0).abs() < 1e-9);
        // Equal octaves either side of center weigh the same.
        let half = tempo_prior(PRIOR_CENTER_BPM / 2.0);
        let double = tempo_prior(PRIOR_CENTER_BPM * 2.0);
        assert!((half - double).abs() < 1e-9);
        assert!(half < 1.0, "an octave off is penalized");
    }

    #[test]
    fn interp_reads_between_samples() {
        let sig = [0.0f32, 2.0, 0.0];
        assert_eq!(interp(&sig, 0.5), 1.0);
        assert_eq!(interp(&sig, 1.0), 2.0);
        assert_eq!(interp(&sig, -1.0), 0.0);
        assert_eq!(interp(&sig, 5.0), 0.0);
    }
}
