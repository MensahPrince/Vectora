//! Sidechain ducking analysis (audio roadmap M8 Phase 4).
//!
//! The pure DSP behind "duck the music under the narration": measure how
//! loud the *voice* lanes are in the speech band over time, turn that into a
//! gain-reduction curve (a classic threshold + attack/release ducker), then
//! thin the curve to the handful of points a volume envelope actually needs.
//!
//! Everything here is pure and sample-domain: it takes mono `f32` and returns
//! plain `Vec`s, with no media, model, or timeline types. The engine owns the
//! decode (via [`AudioReader`](crate::AudioReader)), the per-clip compositing
//! onto a shared timeline, and the mapping of [`reduce_curve`]'s indices into
//! clip-relative keyframe ticks. Keeping the DSP model-free is the same seam
//! the varispeed render uses (`render_stretched_curve` takes a closure rather
//! than a `Clip`), and it makes the tricky parts trivially unit-testable.

/// Control rate of the energy and gain envelopes, in hertz: one analysis step
/// every 10 ms. Ducking moves far slower than audio, so 100 Hz captures every
/// attack/release shape a listener can hear while keeping the curves — and the
/// keyframe reduction — cheap. Both mixers sample volume per audio frame, so
/// this rate never reaches the hot path; it only sets the envelope's detail.
pub const CONTROL_HZ: f32 = 100.0;

/// Speech band, in hertz. Bracketing the voice's fundamental + low formants
/// (and rejecting rumble, handling thumps, and hiss) makes the detector fire
/// on speech rather than on broadband level, which is the whole point of a
/// *sidechain* — music under the voice should keep ducking even where the
/// music itself is louder than the talker.
const BAND_LOW_HZ: f32 = 300.0;
const BAND_HIGH_HZ: f32 = 3400.0;

/// Butterworth Q (maximally flat passband) for the band-edge biquads.
const BAND_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Sidechain ducking settings. Linear units throughout so the DSP never has to
/// reason about decibels; the command/UI layer converts a dB threshold or a
/// percentage if it wants to present those.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DuckSettings {
    /// Speech-band RMS (linear, ~`0..1`) above which the voice counts as
    /// present and the music ducks.
    pub threshold: f32,
    /// Fractional gain reduction held while the voice is present: `0.0` ducks
    /// not at all, `1.0` ducks fully to silence. The ducked floor is
    /// `1.0 - amount`.
    pub amount: f32,
    /// Seconds for the gain to fall toward the ducked floor once the voice
    /// crosses the threshold (`0` snaps instantly).
    pub attack: f32,
    /// Seconds for the gain to recover toward unity once the voice drops back
    /// below the threshold (`0` snaps instantly).
    pub release: f32,
}

impl Default for DuckSettings {
    fn default() -> Self {
        // -32 dBFS ≈ 0.025 linear: above a quiet room tone, below conversational
        // speech. Duck to a third, with a quick 80 ms grab and a gentle 320 ms
        // release — broadcast-typical, and a sane base the UI/agent override.
        Self {
            threshold: 0.025,
            amount: 0.66,
            attack: 0.08,
            release: 0.32,
        }
    }
}

/// One band-edge biquad (RBJ cookbook), Direct Form I. Private to the module:
/// the only thing the band filter exists for is energy, never playback.
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn from_coeffs(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn low_pass(sample_rate: f32, cutoff: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        Self::from_coeffs(
            (1.0 - cos) / 2.0,
            1.0 - cos,
            (1.0 - cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    fn high_pass(sample_rate: f32, cutoff: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        Self::from_coeffs(
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Samples per control step at `sample_rate` (at least one).
fn hop_samples(sample_rate: u32) -> usize {
    ((sample_rate as f32 / CONTROL_HZ).round() as usize).max(1)
}

/// Reduce mono samples to a control-rate speech-band RMS envelope: band-pass
/// the signal (300–3400 Hz), then take the RMS of each `1/CONTROL_HZ`-second
/// hop. One output value per hop, including a final short hop. Empty input
/// yields an empty envelope.
///
/// `sample_rate` is the rate of `mono`. Analysis runs fine well below full
/// fidelity — 16 kHz comfortably covers the band and roughly thirds the work
/// versus 48 kHz — so the engine decodes the voice lanes at a reduced rate.
pub fn speech_band_energy(mono: &[f32], sample_rate: u32) -> Vec<f32> {
    if mono.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let rate = sample_rate as f32;
    let mut hpf = Biquad::high_pass(rate, BAND_LOW_HZ, BAND_Q);
    let mut lpf = Biquad::low_pass(rate, BAND_HIGH_HZ, BAND_Q);

    let hop = hop_samples(sample_rate);
    let mut energy = Vec::with_capacity(mono.len() / hop + 1);
    let mut sum_sq = 0.0f64;
    let mut filled = 0usize;
    for &s in mono {
        let band = lpf.process(hpf.process(s));
        sum_sq += f64::from(band) * f64::from(band);
        filled += 1;
        if filled == hop {
            energy.push((sum_sq / hop as f64).sqrt() as f32);
            sum_sq = 0.0;
            filled = 0;
        }
    }
    if filled > 0 {
        energy.push((sum_sq / filled as f64).sqrt() as f32);
    }
    energy
}

/// One-pole smoothing coefficient for a time constant `tau` seconds at the
/// control rate: `exp(-1 / (CONTROL_HZ · tau))`. `tau <= 0` returns `0` (the
/// gain snaps straight to its target).
fn smoothing_coeff(tau: f32) -> f32 {
    if tau <= 0.0 || !tau.is_finite() {
        return 0.0;
    }
    (-1.0 / (CONTROL_HZ * tau)).exp()
}

/// Turn a (single or composited) speech-band energy envelope into a
/// gain-reduction curve: unity where the voice is below `threshold`, falling
/// toward `1 - amount` where it is above, with separate attack (downward) and
/// release (upward) smoothing. One gain value per energy sample, each in
/// `[1 - amount, 1]`. An empty envelope yields an empty curve.
///
/// The asymmetric one-pole — attack while diving toward the floor, release
/// while climbing back — is the textbook ducker shape and reads naturally as a
/// volume envelope: a quick dip as the talker starts, a slow lift as they
/// pause.
pub fn duck_gain(energy: &[f32], settings: &DuckSettings) -> Vec<f32> {
    let floor = (1.0 - settings.amount).clamp(0.0, 1.0);
    let attack = smoothing_coeff(settings.attack);
    let release = smoothing_coeff(settings.release);

    let mut gain = 1.0f32;
    let mut out = Vec::with_capacity(energy.len());
    for &e in energy {
        let target = if e > settings.threshold { floor } else { 1.0 };
        // Diving toward the floor uses attack; recovering uses release.
        let coeff = if target < gain { attack } else { release };
        gain = target + (gain - target) * coeff;
        out.push(gain);
    }
    out
}

/// Thin a control-rate curve to the fewest `(index, value)` points whose
/// straight-line interpolation stays within `tolerance` of the original
/// (Ramer–Douglas–Peucker, vertical error — the right metric since a volume
/// envelope lerps linearly between keyframes). Endpoints are always kept, so a
/// flat run collapses to its two ends. Fewer than two samples pass through.
///
/// `tolerance` is in gain units; `0` (or negative) keeps every distinguishable
/// vertex.
pub fn reduce_curve(curve: &[f32], tolerance: f32) -> Vec<(usize, f32)> {
    if curve.len() <= 2 {
        return curve.iter().enumerate().map(|(i, &v)| (i, v)).collect();
    }
    let mut keep = vec![false; curve.len()];
    keep[0] = true;
    keep[curve.len() - 1] = true;
    rdp(curve, 0, curve.len() - 1, tolerance.max(0.0), &mut keep);
    keep.iter()
        .enumerate()
        .filter_map(|(i, &k)| k.then_some((i, curve[i])))
        .collect()
}

/// Recursively mark the vertex of greatest vertical deviation from the chord
/// `[lo, hi]`, splitting there when it exceeds `eps`.
fn rdp(curve: &[f32], lo: usize, hi: usize, eps: f32, keep: &mut [bool]) {
    if hi <= lo + 1 {
        return;
    }
    let (y0, y1) = (curve[lo], curve[hi]);
    let span = (hi - lo) as f32;
    let mut worst = 0.0f32;
    let mut worst_i = lo;
    for i in (lo + 1)..hi {
        let t = (i - lo) as f32 / span;
        let line = y0 + (y1 - y0) * t;
        let dist = (curve[i] - line).abs();
        if dist > worst {
            worst = dist;
            worst_i = i;
        }
    }
    if worst > eps {
        keep[worst_i] = true;
        rdp(curve, lo, worst_i, eps, keep);
        rdp(curve, worst_i, hi, eps, keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, amp: f32, sample_rate: u32, secs: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * secs) as usize;
        (0..n)
            .map(|i| {
                amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate as f32).sin()
            })
            .collect()
    }

    fn mean(v: &[f32]) -> f32 {
        v.iter().copied().sum::<f32>() / v.len().max(1) as f32
    }

    // --- speech_band_energy ------------------------------------------------

    #[test]
    fn empty_or_zero_rate_is_empty() {
        assert!(speech_band_energy(&[], 16_000).is_empty());
        assert!(speech_band_energy(&[0.1, 0.2], 0).is_empty());
    }

    #[test]
    fn silence_reads_as_no_energy() {
        let energy = speech_band_energy(&vec![0.0; 16_000], 16_000);
        assert!(!energy.is_empty());
        assert!(energy.iter().all(|&e| e < 1e-6), "silence has no energy");
    }

    #[test]
    fn in_band_tone_passes_out_of_band_is_rejected() {
        let rate = 16_000;
        // 1 kHz sits mid-band; RMS of a 0.5-amp sine ≈ 0.354.
        let mid = speech_band_energy(&tone(1_000.0, 0.5, rate, 0.5), rate);
        let mid = mean(&mid[2..]); // skip filter warm-up hops
        assert!((mid - 0.354).abs() < 0.05, "mid-band ≈ RMS, got {mid}");

        // 50 Hz rumble and 7 kHz hiss are both well outside 300–3400 Hz.
        let low = mean(&speech_band_energy(&tone(50.0, 0.5, rate, 0.5), rate)[2..]);
        let high = mean(&speech_band_energy(&tone(7_000.0, 0.5, rate, 0.5), rate)[2..]);
        assert!(low < mid * 0.2, "low rumble rejected: {low} vs {mid}");
        assert!(high < mid * 0.2, "high hiss rejected: {high} vs {mid}");
    }

    #[test]
    fn envelope_length_tracks_control_rate() {
        let rate = 16_000;
        // 1 s at 100 Hz control rate → ~100 hops.
        let energy = speech_band_energy(&tone(1_000.0, 0.3, rate, 1.0), rate);
        assert!(
            (energy.len() as i32 - 100).abs() <= 1,
            "got {}",
            energy.len()
        );
    }

    // --- duck_gain ---------------------------------------------------------

    #[test]
    fn quiet_voice_never_ducks() {
        let energy = vec![0.001f32; 200];
        let gain = duck_gain(&energy, &DuckSettings::default());
        assert!(
            gain.iter().all(|&g| (g - 1.0).abs() < 1e-6),
            "stays at unity"
        );
    }

    #[test]
    fn loud_voice_ducks_toward_the_floor() {
        let settings = DuckSettings {
            threshold: 0.02,
            amount: 0.66,
            attack: 0.05,
            release: 0.3,
        };
        // A full second of voice present: gain should settle near 1 - amount.
        let energy = vec![0.5f32; 100];
        let gain = duck_gain(&energy, &settings);
        let floor = 1.0 - settings.amount;
        assert!(gain[0] < 1.0, "starts ducking immediately");
        assert!(
            (gain.last().unwrap() - floor).abs() < 0.02,
            "settles near floor {floor}, got {}",
            gain.last().unwrap()
        );
        assert!(gain.iter().all(|&g| g >= floor - 1e-4 && g <= 1.0 + 1e-4));
    }

    #[test]
    fn attack_dives_faster_than_release_recovers() {
        let settings = DuckSettings {
            threshold: 0.02,
            amount: 0.8,
            attack: 0.05,
            release: 0.4,
        };
        // 0.5 s voice, then 0.5 s silence.
        let mut energy = vec![0.5f32; 50];
        energy.extend(std::iter::repeat(0.0).take(50));
        let gain = duck_gain(&energy, &settings);
        // Down move within the first few steps is larger than the up move over
        // the same number of steps after the voice stops.
        let attack_drop = gain[0] - gain[5];
        let release_rise = gain[55] - gain[50];
        assert!(
            attack_drop > release_rise,
            "{attack_drop} vs {release_rise}"
        );
    }

    #[test]
    fn zero_attack_snaps_instantly() {
        let settings = DuckSettings {
            threshold: 0.02,
            amount: 0.5,
            attack: 0.0,
            release: 0.0,
        };
        let energy = vec![0.5f32, 0.5, 0.0];
        let gain = duck_gain(&energy, &settings);
        assert!((gain[0] - 0.5).abs() < 1e-6, "snaps to floor");
        assert!((gain[2] - 1.0).abs() < 1e-6, "snaps back to unity");
    }

    // --- reduce_curve ------------------------------------------------------

    #[test]
    fn flat_curve_collapses_to_endpoints() {
        let pts = reduce_curve(&[1.0; 50], 0.01);
        assert_eq!(pts, vec![(0, 1.0), (49, 1.0)]);
    }

    #[test]
    fn straight_ramp_keeps_only_ends() {
        let ramp: Vec<f32> = (0..21).map(|i| i as f32 / 20.0).collect();
        let pts = reduce_curve(&ramp, 0.01);
        assert_eq!(pts.len(), 2, "a line needs no interior points");
        assert_eq!(pts.first().unwrap().0, 0);
        assert_eq!(pts.last().unwrap().0, 20);
    }

    #[test]
    fn dip_keeps_its_apex() {
        // Down to 0.2 at the middle, back to 1.0 — a V.
        let mut curve = vec![1.0f32; 21];
        for (i, c) in curve.iter_mut().enumerate() {
            *c = if i <= 10 {
                1.0 - 0.8 * (i as f32 / 10.0)
            } else {
                0.2 + 0.8 * ((i - 10) as f32 / 10.0)
            };
        }
        let pts = reduce_curve(&curve, 0.01);
        assert!(pts.iter().any(|&(i, _)| i == 10), "apex kept: {pts:?}");
        // Endpoints plus the apex — the three vertices of the V.
        assert_eq!(pts.len(), 3);
    }

    #[test]
    fn tolerance_trades_points_for_fidelity() {
        // Noisy curve: a looser tolerance keeps fewer points.
        let curve: Vec<f32> = (0..100)
            .map(|i| 0.5 + 0.4 * (i as f32 * 0.3).sin())
            .collect();
        let tight = reduce_curve(&curve, 0.01).len();
        let loose = reduce_curve(&curve, 0.2).len();
        assert!(loose < tight, "looser keeps fewer: {loose} < {tight}");
        assert!(loose >= 2);
    }

    #[test]
    fn short_curves_pass_through() {
        assert_eq!(reduce_curve(&[], 0.1), vec![]);
        assert_eq!(reduce_curve(&[0.5], 0.1), vec![(0, 0.5)]);
        assert_eq!(reduce_curve(&[0.5, 0.7], 0.1), vec![(0, 0.5), (1, 0.7)]);
    }
}
