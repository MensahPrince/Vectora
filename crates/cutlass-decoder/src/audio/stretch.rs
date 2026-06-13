//! Offline varispeed render: time-stretch a clip's source window to a target
//! timeline length, preserving pitch (or shifting it for "chipmunk" mode).
//!
//! This is the backend the audio mixers use to make retimed clips audible
//! (audio roadmap M8 Phase 3). The realtime preview mixer and the export
//! mixer both call [`render_stretched`] with the *same* inputs, so what you
//! hear while scrubbing is what you ship.
//!
//! Backend: [Signalsmith Stretch](https://signalsmith-audio.co.uk/code/stretch/)
//! (MIT, header-only C++ with a thin Rust wrapper) — chosen over rubberband
//! (GPL) and FFmpeg's stretch filters so Cutlass keeps its MIT/Apache license
//! posture. The render is one-shot ([`Stretch::exact`]) so latency is
//! compensated internally and the whole span resolves deterministically; the
//! mixers cache the result like a proxy and serve it 1:1.

use std::path::Path;

use signalsmith_stretch::Stretch;

use crate::audio::playback::{AudioReader, CHANNELS};
use crate::error::DecodeError;

/// Render the source window `[src_start_frame, src_start_frame + src_frames)`
/// (output sample frames at `out_rate`) time-stretched to exactly `out_frames`
/// of interleaved stereo output.
///
/// - `reversed` walks the source window back-to-front (CapCut reverse).
/// - `transpose` is a frequency multiplier: `1.0` preserves pitch (the
///   varispeed default), `> 1.0` shifts up (the optional "chipmunk" mode where
///   pitch follows speed). For a clip sped up by `s×`, pass `s` to follow the
///   pitch or `1.0` to lock it.
///
/// Returns `out_frames * CHANNELS` samples (zero-padded if the source runs
/// short). A zero-length window or output yields silence.
pub fn render_stretched(
    path: &Path,
    out_rate: u32,
    src_start_frame: i64,
    src_frames: i64,
    out_frames: i64,
    reversed: bool,
    transpose: f32,
) -> Result<Vec<f32>, DecodeError> {
    let out_frames = out_frames.max(0) as usize;
    let output_len = out_frames * CHANNELS;
    if out_frames == 0 || src_frames <= 0 {
        return Ok(vec![0.0; output_len]);
    }

    let input = read_source_window(path, out_rate, src_start_frame, src_frames, reversed)?;

    let mut stretch = Stretch::preset_default(CHANNELS as u32, out_rate);
    if (transpose - 1.0).abs() > f32::EPSILON {
        stretch.set_transpose_factor(transpose, None);
    }
    let mut output = vec![0.0; output_len];
    stretch.exact(&input, &mut output);
    Ok(output)
}

/// Like [`render_stretched`] but with a *time-varying* rate (M2 speed curves):
/// `src_fraction` maps an output-position fraction `p ∈ [0, 1]` to the fraction
/// of the source window that should have played by then. It must be monotonic
/// with `src_fraction(0) == 0` and `src_fraction(1) == 1` — exactly the
/// normalized cumulative integral of the clip's speed ramp
/// (`cutlass_models::speed_curve_source_fraction`), so the audio warps in step
/// with the video the ramp already drives.
///
/// The render is a single continuous phase-vocoder pass (no per-segment clicks):
/// it reproduces [`Stretch::exact`]'s latency compensation — seek-prime, then
/// fold-back + shuffle + flush around the output-latency delay — but feeds the
/// interior in blocks whose input/output ratio tracks the curve. The whole span
/// resolves into one buffer the mixers cache and serve 1:1, same as the
/// constant path.
pub fn render_stretched_curve<F>(
    path: &Path,
    out_rate: u32,
    src_start_frame: i64,
    src_frames: i64,
    out_frames: i64,
    reversed: bool,
    transpose: f32,
    src_fraction: F,
) -> Result<Vec<f32>, DecodeError>
where
    F: Fn(f64) -> f64,
{
    let out_frames = out_frames.max(0) as usize;
    let output_len = out_frames * CHANNELS;
    if out_frames == 0 || src_frames <= 0 {
        return Ok(vec![0.0; output_len]);
    }

    let input = read_source_window(path, out_rate, src_start_frame, src_frames, reversed)?;
    let in_frames = input.len() / CHANNELS;

    let mut stretch = Stretch::preset_default(CHANNELS as u32, out_rate);
    if (transpose - 1.0).abs() > f32::EPSILON {
        stretch.set_transpose_factor(transpose, None);
    }
    let mut output = vec![0.0; output_len];

    let l_in = stretch.input_latency();
    let l_out = stretch.output_latency();

    // Too short for latency-compensated streaming (`exact` needs at least two
    // output-latencies of room): fall back to a uniform stretch. The ramp
    // shaping is negligible over a sub-`2·l_out` span (~a few hundred ms).
    if out_frames < 2 * l_out {
        stretch.exact(&input, &mut output);
        return Ok(output);
    }

    // `exact` reads its process input offset by `l_in` and zero-padded past the
    // end (the first `l_in` source frames are fed as seek pre-roll, and a
    // trailing `l_in` of silence flushes the processing time to the end). Build
    // that view explicitly so the variable-rate loop can slice it per block.
    let mut zpi = vec![0.0f32; in_frames * CHANNELS];
    for i in 0..in_frames {
        let src = l_in + i;
        if src < in_frames {
            zpi[i * CHANNELS..(i + 1) * CHANNELS]
                .copy_from_slice(&input[src * CHANNELS..(src + 1) * CHANNELS]);
        }
    }

    // Prime on the centre of the input, exactly like `exact` (rate is only a
    // phase-prediction hint, so the overall ratio is fine for a ramp too).
    let prime = l_in.min(in_frames);
    stretch.seek(&input[..prime * CHANNELS], in_frames as f64 / out_frames as f64);

    // Cumulative source frames the processor should have consumed by output
    // frame `o` (0 → `in_frames` over 0 → `out_frames`), following the curve.
    let cin = |o: usize| -> usize {
        let p = o as f64 / out_frames as f64;
        let frac = src_fraction(p).clamp(0.0, 1.0);
        ((in_frames as f64) * frac).round() as usize
    };

    // Walk the output in fixed blocks; each block's input span is the curve's
    // source delta over it, so the local stretch ratio rides the ramp. The
    // chunks sum to (`in_frames`, `out_frames`) — identical coverage to
    // `exact`'s single process call, just split to follow the rate.
    const BLOCK: usize = 1024;
    let mut o = 0usize;
    let mut consumed = 0usize;
    while o < out_frames {
        let o1 = (o + BLOCK).min(out_frames);
        let in1 = cin(o1).clamp(consumed, in_frames);
        stretch.process(
            &zpi[consumed * CHANNELS..in1 * CHANNELS],
            &mut output[o * CHANNELS..o1 * CHANNELS],
        );
        consumed = in1;
        o = o1;
    }

    compensate_output_latency(&mut output, l_out);
    // Drain the final `l_out` frames straight into the tail, where `exact`'s
    // fold-back-within-flush expects the shuffled remainder to live.
    let base = (out_frames - l_out) * CHANNELS;
    stretch.flush(&mut output[base..]);
    Ok(output)
}

/// Reproduce the tail of [`Stretch::exact`]: fold the first block back onto
/// itself, then shuffle everything left by `l_out` to compensate for the
/// output-latency delay. Operates on an interleaved stereo buffer in place.
fn compensate_output_latency(output: &mut [f32], l_out: usize) {
    let out_frames = output.len() / CHANNELS;
    if l_out == 0 || out_frames <= l_out {
        return;
    }
    // Fold the leading transient back onto itself.
    for i in 0..(out_frames - l_out).min(l_out) {
        for ch in 0..CHANNELS {
            output[(i + l_out) * CHANNELS + ch] -= output[(l_out - 1 - i) * CHANNELS + ch];
        }
    }
    // Shuffle along by the output latency.
    for i in 0..(out_frames - l_out) {
        for ch in 0..CHANNELS {
            output[i * CHANNELS + ch] = output[(i + l_out) * CHANNELS + ch];
        }
    }
}

/// Decode `src_frames` of interleaved stereo from `path` starting at output
/// frame `src_start_frame` (at `out_rate`), zero-padding a short tail or a
/// lead gap, and reversing the frame order when `reversed`.
fn read_source_window(
    path: &Path,
    out_rate: u32,
    src_start_frame: i64,
    src_frames: i64,
    reversed: bool,
) -> Result<Vec<f32>, DecodeError> {
    let frames = src_frames.max(0) as usize;
    let mut buf = vec![0.0f32; frames * CHANNELS];

    let mut reader = AudioReader::open(path, out_rate)?;
    reader.seek_to_frame(src_start_frame)?;
    // A stream that starts after the requested point leaves a lead gap; the
    // padding already in `buf` covers it (same handling as the mixers).
    let lead = reader
        .position()
        .map_or(0, |p| (p - src_start_frame).clamp(0, src_frames) as usize);

    let mut filled = lead;
    while filled < frames {
        let got = reader.read(&mut buf[filled * CHANNELS..])?;
        if got == 0 {
            break; // source exhausted: rest stays silent
        }
        filled += got;
    }

    if reversed {
        reverse_frames(&mut buf);
    }
    Ok(buf)
}

/// Reverse the frame order of an interleaved stereo buffer in place (each
/// `(L, R)` pair stays intact; only their order flips).
fn reverse_frames(buf: &mut [f32]) {
    let frames = buf.len() / CHANNELS;
    for i in 0..frames / 2 {
        let j = frames - 1 - i;
        for ch in 0..CHANNELS {
            buf.swap(i * CHANNELS + ch, j * CHANNELS + ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const RATE: u32 = 48_000;

    fn audio_asset() -> Option<PathBuf> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "mp3"))
    }

    #[test]
    fn reverse_frames_flips_pairs() {
        // Two stereo frames (L,R): [(1,2),(3,4)] reverses to [(3,4),(1,2)].
        let mut buf = vec![1.0, 2.0, 3.0, 4.0];
        reverse_frames(&mut buf);
        assert_eq!(buf, vec![3.0, 4.0, 1.0, 2.0]);
    }

    #[test]
    fn zero_output_is_silence() {
        let out = render_stretched(Path::new("/nope.mp3"), RATE, 0, 48_000, 0, false, 1.0)
            .expect("zero output never touches the file");
        assert!(out.is_empty());
    }

    #[test]
    fn render_fills_the_requested_output_length() {
        let Some(path) = audio_asset() else {
            return;
        };
        // 1s of source (48000 frames) slowed to 2s (96000 frames): a ½×
        // speed clip. Output must be exactly the requested length and audible.
        let out = render_stretched(&path, RATE, 0, 48_000, 96_000, false, 1.0).expect("render");
        assert_eq!(out.len(), 96_000 * CHANNELS);
        assert!(out.iter().any(|&s| s != 0.0), "stretched audio is not silent");
        assert!(out.iter().all(|&s| s.is_finite()), "no NaNs/infs");
    }

    #[test]
    fn reversed_render_is_audible_and_distinct() {
        let Some(path) = audio_asset() else {
            return;
        };
        let forward = render_stretched(&path, RATE, 0, 48_000, 48_000, false, 1.0).expect("fwd");
        let reversed = render_stretched(&path, RATE, 0, 48_000, 48_000, true, 1.0).expect("rev");
        assert_eq!(forward.len(), reversed.len());
        assert!(reversed.iter().any(|&s| s != 0.0), "reversed audio plays");
        // Reversing real audio changes the waveform.
        let differ = forward
            .iter()
            .zip(&reversed)
            .any(|(a, b)| (a - b).abs() > 1e-3);
        assert!(differ, "reversed output differs from forward");
    }

    #[test]
    fn curve_zero_output_is_silence() {
        let out =
            render_stretched_curve(Path::new("/nope.mp3"), RATE, 0, 48_000, 0, false, 1.0, |p| p)
                .expect("zero output never touches the file");
        assert!(out.is_empty());
    }

    #[test]
    fn curve_render_with_identity_fraction_tracks_the_constant_render() {
        let Some(path) = audio_asset() else {
            return;
        };
        // An identity fraction is a constant rate, so the variable-rate path
        // should land in the same ballpark as the proven `exact` constant path
        // (not byte-identical — the streaming head/tail handling differs).
        let curve =
            render_stretched_curve(&path, RATE, 0, 48_000, 96_000, false, 1.0, |p| p).expect("curve");
        let constant = render_stretched(&path, RATE, 0, 48_000, 96_000, false, 1.0).expect("const");
        assert_eq!(curve.len(), constant.len());
        assert!(curve.iter().any(|&s| s != 0.0), "curve render is audible");
        assert!(curve.iter().all(|&s| s.is_finite()), "no NaNs/infs");
        let energy = |b: &[f32]| b.iter().map(|s| f64::from(s * s)).sum::<f64>();
        let (ce, ke) = (energy(&constant), energy(&curve));
        if ce > 0.0 {
            // Same order of magnitude: the streaming pass isn't dropping or
            // doubling the signal.
            assert!(ke > ce * 0.2 && ke < ce * 5.0, "energy {ke} vs constant {ce}");
        }
    }

    #[test]
    fn curve_render_follows_a_ramp_and_stays_finite() {
        let Some(path) = audio_asset() else {
            return;
        };
        // A slow-then-fast ramp: monotonic, 0→0, 1→1, source advances as p².
        let out =
            render_stretched_curve(&path, RATE, 0, 96_000, 96_000, false, 1.0, |p| p * p).expect("ramp");
        assert_eq!(out.len(), 96_000 * CHANNELS);
        assert!(out.iter().any(|&s| s != 0.0), "ramped audio plays");
        assert!(out.iter().all(|&s| s.is_finite()), "no NaNs/infs");
    }

    #[test]
    fn curve_reversed_render_is_audible() {
        let Some(path) = audio_asset() else {
            return;
        };
        let out = render_stretched_curve(&path, RATE, 0, 48_000, 48_000, true, 1.0, |p| p)
            .expect("reversed ramp");
        assert!(out.iter().any(|&s| s != 0.0), "reversed ramp plays");
        assert!(out.iter().all(|&s| s.is_finite()), "no NaNs/infs");
    }
}
