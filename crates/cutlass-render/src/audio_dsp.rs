//! Audio DSP helpers for the export mixer: RNNoise denoising and varispeed
//! source-frame mapping.

use cutlass_core::{AudioReader, DecodeError};
use cutlass_models::{Param, speed_curve_integral};
use nnnoiseless::DenoiseState;

const I16_SCALE: f32 = 32768.0;

fn to_rnnoise(sample: f32) -> f32 {
    (sample * I16_SCALE).clamp(-32768.0, 32767.0)
}

fn from_rnnoise(sample: f32) -> f32 {
    (sample / I16_SCALE).clamp(-1.0, 1.0)
}

/// How a span maps clip-relative output samples to fractional source offsets.
#[derive(Debug, Clone)]
pub(crate) enum SpanWarp {
    /// Unity speed, no ramp: one output sample ↔ one source sample.
    Linear,
    /// Constant speed multiplier (`source_offset = output_offset × num/den`).
    FlatSpeed { num: i32, den: i32 },
    /// Speed-curve ramp: integral ratio over the source window (matches video
    /// [`cutlass_models::Clip::source_time_at`]).
    Curved {
        curve: Param<f32>,
        source_len: i64,
        curve_total: f64,
    },
}

/// Fractional source offset from the span's source window start for a
/// clip-relative output sample `rel_out` (`0` = span start).
pub(crate) fn warped_source_frame(warp: &SpanWarp, rel_out: i64, span_len: i64) -> f64 {
    match warp {
        SpanWarp::Linear => rel_out as f64,
        SpanWarp::FlatSpeed { num, den } => rel_out as f64 * f64::from(*num) / f64::from(*den),
        SpanWarp::Curved {
            curve,
            source_len,
            curve_total,
        } => {
            let p = rel_out as f64 / span_len.max(1) as f64;
            let ratio = if *curve_total > 0.0 {
                speed_curve_integral(curve, p) / *curve_total
            } else {
                p
            };
            *source_len as f64 * ratio
        }
    }
}

/// Pull-based reader that runs RNNoise on each channel at 48 kHz.
pub(crate) struct DenoiseReader {
    inner: Box<dyn AudioReader>,
    channels: usize,
    states: Vec<Box<DenoiseState<'static>>>,
    /// Denoised interleaved samples waiting for the caller.
    pending: Vec<f32>,
    /// Per-channel PCM not yet fed to RNNoise.
    channel_in: Vec<Vec<f32>>,
    out_bufs: Vec<[f32; DenoiseState::FRAME_SIZE]>,
    in_bufs: Vec<[f32; DenoiseState::FRAME_SIZE]>,
    /// RNNoise's first output frame has fade-in artifacts — discard it.
    first_output: bool,
    raw_scratch: Vec<f32>,
}

impl DenoiseReader {
    pub(crate) fn new(inner: Box<dyn AudioReader>, channels: usize) -> Self {
        let states = (0..channels).map(|_| DenoiseState::new()).collect();
        Self {
            inner,
            channels,
            states,
            pending: Vec::new(),
            channel_in: (0..channels).map(|_| Vec::new()).collect(),
            out_bufs: (0..channels)
                .map(|_| [0.0; DenoiseState::FRAME_SIZE])
                .collect(),
            in_bufs: (0..channels)
                .map(|_| [0.0; DenoiseState::FRAME_SIZE])
                .collect(),
            first_output: true,
            raw_scratch: Vec::new(),
        }
    }

    fn reset_denoise(&mut self) {
        self.states = (0..self.channels).map(|_| DenoiseState::new()).collect();
        self.pending.clear();
        for ch in &mut self.channel_in {
            ch.clear();
        }
        self.first_output = true;
    }

    fn process_ready_frames(&mut self) {
        let frame = DenoiseState::FRAME_SIZE;
        loop {
            if self.channel_in.iter().any(|ch| ch.len() < frame) {
                break;
            }
            for ch in 0..self.channels {
                for (dst, src) in self.in_bufs[ch]
                    .iter_mut()
                    .zip(self.channel_in[ch].drain(..frame).map(to_rnnoise))
                {
                    *dst = src;
                }
                self.states[ch].process_frame(&mut self.out_bufs[ch], &self.in_bufs[ch]);
            }
            if !self.first_output {
                for i in 0..frame {
                    for ch in 0..self.channels {
                        self.pending.push(from_rnnoise(self.out_bufs[ch][i]));
                    }
                }
            }
            self.first_output = false;
        }
    }

    fn copy_pending(&mut self, out: &mut [f32]) -> usize {
        let take = (out.len() / self.channels).min(self.pending.len() / self.channels);
        if take == 0 {
            return 0;
        }
        let bytes = take * self.channels;
        out[..bytes].copy_from_slice(&self.pending[..bytes]);
        self.pending.drain(..bytes);
        take
    }
}

impl AudioReader for DenoiseReader {
    fn read(&mut self, out: &mut [f32]) -> Result<usize, DecodeError> {
        let want_frames = out.len() / self.channels;
        let mut written = 0;
        while written < want_frames {
            let copied = self.copy_pending(&mut out[written * self.channels..]);
            written += copied;
            if written >= want_frames {
                break;
            }
            let chunk_frames = DenoiseState::FRAME_SIZE.max(want_frames - written);
            self.raw_scratch.resize(chunk_frames * self.channels, 0.0);
            let got = self.inner.read(&mut self.raw_scratch)?;
            if got == 0 {
                break;
            }
            let end = got * self.channels;
            for idx in 0..end {
                let sample = self.raw_scratch[idx];
                self.channel_in[idx % self.channels].push(sample);
            }
            self.process_ready_frames();
        }
        Ok(written)
    }

    fn seek_to_frame(&mut self, frame: i64) -> Result<(), DecodeError> {
        self.reset_denoise();
        self.inner.seek_to_frame(frame)
    }

    fn position(&self) -> Option<i64> {
        self.inner.position()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    struct SineReader {
        pos: i64,
        channels: usize,
        rate: u32,
        freq: f32,
    }

    impl SineReader {
        fn new(rate: u32, channels: usize, freq: f32) -> Self {
            Self {
                pos: 0,
                channels,
                rate,
                freq,
            }
        }
    }

    impl AudioReader for SineReader {
        fn read(&mut self, out: &mut [f32]) -> Result<usize, DecodeError> {
            let frames = out.len() / self.channels;
            for f in 0..frames {
                let t = (self.pos + f as i64) as f32 / self.rate as f32;
                let sample = (2.0 * PI * self.freq * t).sin() * 0.5;
                for ch in 0..self.channels {
                    out[f * self.channels + ch] = sample;
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
    fn warped_source_frame_flat_speed() {
        let warp = SpanWarp::FlatSpeed { num: 2, den: 1 };
        assert!((warped_source_frame(&warp, 1000, 2000) - 2000.0).abs() < 1e-6);
        let warp = SpanWarp::FlatSpeed { num: 1, den: 2 };
        assert!((warped_source_frame(&warp, 1000, 2000) - 500.0).abs() < 1e-6);
    }

    #[test]
    fn warped_source_frame_curve_endpoints() {
        let curve = Param::Constant(2.0);
        let warp = SpanWarp::Curved {
            curve,
            source_len: 4800,
            curve_total: 2.0,
        };
        assert!((warped_source_frame(&warp, 0, 2400) - 0.0).abs() < 1e-6);
        assert!((warped_source_frame(&warp, 2400, 2400) - 4800.0).abs() < 1e-3);
    }

    #[test]
    fn denoise_reader_preserves_length_across_odd_blocks() {
        let inner = Box::new(SineReader::new(48_000, 2, 440.0));
        let mut reader = DenoiseReader::new(inner, 2);
        let mut out = vec![0.0; 777 * 2];
        let got = reader.read(&mut out).unwrap();
        assert_eq!(got, 777);
        assert!(out.iter().any(|&s| s.abs() > 0.001));
    }

    #[test]
    fn denoise_reader_seek_resets_state() {
        let inner = Box::new(SineReader::new(48_000, 2, 440.0));
        let mut reader = DenoiseReader::new(inner, 2);
        let mut block = vec![0.0; 512 * 2];
        reader.read(&mut block).unwrap();
        reader.seek_to_frame(0).unwrap();
        let mut again = vec![0.0; 512 * 2];
        reader.read(&mut again).unwrap();
        assert!(again.iter().any(|&s| s.is_finite()));
    }
}
