//! Offline noise reduction: run a clip's source window through RNNoise so the
//! mixers can serve a de-hissed/de-hummed version (audio roadmap M8 Phase 5).
//!
//! Backend: [`nnnoiseless`](https://crates.io/crates/nnnoiseless), a pure-Rust
//! port of Xiph's RNNoise — a recurrent network trained to keep speech and
//! suppress steady background noise (fan hum, hiss, room tone). Chosen over a
//! C binding so Cutlass stays pure Rust and keeps its MIT/Apache posture.
//!
//! Like the varispeed render ([`crate::render_stretched`]) this is a one-shot,
//! whole-window pass: the realtime preview mixer and the export mixer both call
//! it with the *same* inputs and cache the result, so what you hear while
//! scrubbing is what you ship. RNNoise is tuned for 48 kHz; the render runs at
//! whatever rate the caller decodes at (export is always 48 kHz, preview is the
//! device rate), matching the convention the stretch render already uses — at
//! off-spec rates the suppression is a touch less precise but still effective.

use std::path::Path;

use nnnoiseless::DenoiseState;

use crate::audio::playback::CHANNELS;
use crate::audio::stretch::read_source_window;
use crate::error::DecodeError;

/// RNNoise works on `f32`s scaled to the `i16` range (`±32768`), not the
/// `[-1, 1]` PCM the mixers use — scale in on the way down, out on the way up.
const I16_SCALE: f32 = 32_768.0;

/// Render the source window `[src_start_frame, src_start_frame + src_frames)`
/// (output sample frames at `out_rate`) denoised, as `out_frames` of
/// interleaved stereo. Denoise is a 1× effect, so `src_frames == out_frames`
/// in practice; the buffer is padded/truncated to `out_frames` regardless.
///
/// `reversed` walks the window back-to-front (a reversed clip that is also
/// denoised). A zero-length window or output yields silence.
pub fn render_denoised(
    path: &Path,
    out_rate: u32,
    src_start_frame: i64,
    src_frames: i64,
    out_frames: i64,
    reversed: bool,
) -> Result<Vec<f32>, DecodeError> {
    let out_frames = out_frames.max(0) as usize;
    let output_len = out_frames * CHANNELS;
    if out_frames == 0 || src_frames <= 0 {
        return Ok(vec![0.0; output_len]);
    }

    let mut buf = read_source_window(path, out_rate, src_start_frame, src_frames, reversed)?;
    denoise_interleaved(&mut buf);
    buf.resize(output_len, 0.0);
    Ok(buf)
}

/// Denoise an interleaved stereo buffer in place, each channel through its own
/// RNNoise state (the two ears are independent). Length is preserved.
///
/// Used both for denoise-only spans (via [`render_denoised`]) and to clean a
/// retimed span's time-stretched buffer (denoise rides on top of varispeed).
pub fn denoise_interleaved(buf: &mut [f32]) {
    let frames = buf.len() / CHANNELS;
    if frames == 0 {
        return;
    }
    let mut mono = vec![0.0f32; frames];
    for ch in 0..CHANNELS {
        for (f, m) in mono.iter_mut().enumerate() {
            *m = buf[f * CHANNELS + ch];
        }
        let denoised = denoise_mono(&mono);
        for (f, d) in denoised.iter().enumerate() {
            buf[f * CHANNELS + ch] = *d;
        }
    }
}

/// Denoise one mono channel, returning a same-length buffer.
///
/// RNNoise processes `FRAME_SIZE` (480) samples at a time with a one-frame
/// overlap-add latency: the output of call *i* covers input frame *i-1*, and
/// the very first output is a windowing fade-in artifact. To get an aligned,
/// same-length result we feed every input frame (zero-padding the last) plus
/// one trailing silent flush frame, then drop that first output — leaving one
/// denoised output frame per input frame.
fn denoise_mono(input: &[f32]) -> Vec<f32> {
    let n = input.len();
    if n == 0 {
        return Vec::new();
    }
    let fs = DenoiseState::FRAME_SIZE;
    let nframes = n.div_ceil(fs);

    let mut state = DenoiseState::new();
    let mut frame_in = vec![0.0f32; fs];
    let mut frame_out = vec![0.0f32; fs];
    let mut result = Vec::with_capacity(nframes * fs);

    // `nframes` real frames + 1 flush frame; the leading output is discarded.
    for i in 0..=nframes {
        if i < nframes {
            let base = i * fs;
            for (k, slot) in frame_in.iter_mut().enumerate() {
                let s = base + k;
                *slot = if s < n { input[s] * I16_SCALE } else { 0.0 };
            }
        } else {
            frame_in.fill(0.0);
        }
        state.process_frame(&mut frame_out, &frame_in);
        if i >= 1 {
            result.extend(frame_out.iter().map(|&s| s / I16_SCALE));
        }
    }
    result.truncate(n);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const RATE: u32 = 48_000;

    fn audio_asset() -> Option<PathBuf> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local-assets/assets");
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "mp3"))
    }

    fn energy(buf: &[f32]) -> f64 {
        buf.iter().map(|&s| f64::from(s) * f64::from(s)).sum()
    }

    #[test]
    fn denoise_mono_preserves_length_and_silence() {
        // Silence in → silence out, exact length (covers the sub-frame and
        // multi-frame tails of the chunking + flush).
        for n in [0usize, 1, 479, 480, 481, 961, 5_000] {
            let out = denoise_mono(&vec![0.0f32; n]);
            assert_eq!(out.len(), n, "length preserved for n={n}");
            assert!(out.iter().all(|&s| s == 0.0), "silence stays silent");
        }
    }

    #[test]
    fn denoise_attenuates_broadband_noise() {
        // White-ish noise carries no speech, so RNNoise should clamp its gains
        // and drop the energy. A cheap LCG keeps the test dependency-free and
        // deterministic.
        let mut seed = 0x1234_5678u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect();
        let out = denoise_mono(&noise);
        assert_eq!(out.len(), noise.len());
        assert!(out.iter().all(|s| s.is_finite()), "no NaNs/infs");
        assert!(
            energy(&out) < energy(&noise) * 0.8,
            "broadband noise is suppressed (out {} vs in {})",
            energy(&out),
            energy(&noise)
        );
    }

    #[test]
    fn denoise_interleaved_runs_both_channels() {
        // A two-channel buffer where the channels differ stays the right length
        // and finite after denoising, both ears touched.
        let frames = 2_000;
        let mut buf = vec![0.0f32; frames * CHANNELS];
        let mut seed = 99u32;
        for f in 0..frames {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let n = (seed >> 9) as f32 / (1u32 << 22) as f32 - 1.0;
            buf[f * CHANNELS] = n;
            buf[f * CHANNELS + 1] = -n;
        }
        denoise_interleaved(&mut buf);
        assert_eq!(buf.len(), frames * CHANNELS);
        assert!(buf.iter().all(|s| s.is_finite()), "no NaNs/infs");
    }

    #[test]
    fn render_zero_output_is_silence() {
        let out = render_denoised(Path::new("/nope.mp3"), RATE, 0, 48_000, 0, false)
            .expect("zero output never touches the file");
        assert!(out.is_empty());
    }

    #[test]
    fn render_denoised_fills_the_window_and_stays_finite() {
        let Some(path) = audio_asset() else {
            return;
        };
        let out = render_denoised(&path, RATE, 0, 48_000, 48_000, false).expect("render");
        assert_eq!(out.len(), 48_000 * CHANNELS);
        assert!(out.iter().all(|s| s.is_finite()), "no NaNs/infs");
    }
}
