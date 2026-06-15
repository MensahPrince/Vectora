//! Local transcription via whisper.cpp — the opt-in `whisper` feature.
//!
//! [`WhisperTranscriber`] loads a ggml model with `whisper-rs` and runs the
//! full pipeline on a `&[f32]` mono buffer, returning word-timed [`Transcript`]
//! data. Built only with `--features whisper`, which pulls the C/C++ + cmake
//! toolchain, so the default editor build and CI never see it. Fetch a model
//! first via [`crate::models::ModelCache`] (see the whisper registry there).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::transcribe::{
    Segment, Transcribe, TranscribeError, TranscribeOptions, Transcript, Word,
};

/// whisper.cpp expects 16 kHz mono PCM; the caller resamples to this rate.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// A whisper.cpp-backed transcriber holding one loaded model context. Cheap to
/// reuse across clips — `transcribe` creates a fresh state per call.
pub struct WhisperTranscriber {
    context: WhisperContext,
    threads: usize,
}

impl WhisperTranscriber {
    /// Load a ggml model from `path` (fetch one with [`crate::models`]).
    pub fn from_model_path(path: &Path) -> Result<Self, TranscribeError> {
        let context = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| TranscribeError::ModelUnavailable(format!("{}: {e}", path.display())))?;
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Ok(Self { context, threads })
    }

    /// Override the inference thread count (defaults to available parallelism).
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }
}

impl Transcribe for WhisperTranscriber {
    fn transcribe(
        &self,
        audio: &[f32],
        sample_rate: u32,
        options: &TranscribeOptions,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(f32),
    ) -> Result<Transcript, TranscribeError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(TranscribeError::Cancelled);
        }
        if sample_rate != WHISPER_SAMPLE_RATE {
            return Err(TranscribeError::Backend(format!(
                "whisper expects {WHISPER_SAMPLE_RATE} Hz mono audio, got {sample_rate} Hz; \
                 resample before transcribing"
            )));
        }
        if audio.is_empty() {
            return Ok(Transcript::default());
        }

        let mut state = self
            .context
            .create_state()
            .map_err(|e| TranscribeError::Backend(e.to_string()))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads as i32);
        params.set_translate(options.translate);
        params.set_language(options.language.as_deref());
        // Word-level timing is the whole point — it's what captions and the
        // transcript editor map onto source ticks.
        params.set_token_timestamps(true);
        // whisper.cpp's own stdout/stderr printing is noise in a GUI app.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // Responsive cancel: whisper polls the abort callback inside its decode
        // loop, on this same (synchronous) thread, so the borrowed `cancel`
        // outlives every call. `*const AtomicBool` is a 'static type (no
        // lifetime parameter), so the closure meets the `'static` bound without
        // erasing any lifetime.
        let cancel_ptr: *const AtomicBool = cancel;
        params.set_abort_callback_safe(move || {
            // SAFETY: `cancel` is borrowed for the whole `full()` call below —
            // exactly when this callback runs — so the pointer stays valid.
            unsafe { (*cancel_ptr).load(Ordering::Relaxed) }
        });

        on_progress(0.0);
        state.full(params, audio).map_err(|e| {
            // whisper returns a generic error when the abort callback fires;
            // report that as a clean cancel rather than a backend failure.
            if cancel.load(Ordering::Relaxed) {
                TranscribeError::Cancelled
            } else {
                TranscribeError::Backend(e.to_string())
            }
        })?;
        on_progress(1.0);

        Ok(build_transcript(&state))
    }
}

/// Centiseconds (whisper's timestamp unit) → seconds.
fn cs_to_secs(cs: i64) -> f64 {
    cs as f64 / 100.0
}

/// Assemble our [`Transcript`] from whisper's segments and per-token
/// timestamps, dropping whisper's special tokens (`[_BEG_]`, `[_TT_..]`, …).
fn build_transcript(state: &WhisperState) -> Transcript {
    let mut segments = Vec::new();
    for seg in state.as_iter() {
        let text = seg
            .to_str_lossy()
            .map(|c| c.into_owned())
            .unwrap_or_default();
        let mut words = Vec::new();
        for t in 0..seg.n_tokens() {
            let Some(token) = seg.get_token(t) else {
                continue;
            };
            let piece = token
                .to_str_lossy()
                .map(|c| c.into_owned())
                .unwrap_or_default();
            if piece.starts_with("[_") {
                continue;
            }
            let data = token.token_data();
            words.push(Word {
                text: piece,
                start: cs_to_secs(data.t0),
                end: cs_to_secs(data.t1),
                confidence: Some(token.token_probability()),
            });
        }
        segments.push(Segment {
            text,
            start: cs_to_secs(seg.start_timestamp()),
            end: cs_to_secs(seg.end_timestamp()),
            words,
        });
    }
    Transcript {
        segments,
        language: None,
    }
}
