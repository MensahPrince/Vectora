//! Offline speech transcription for Cutlass.
//!
//! This crate has two deliberately separate responsibilities:
//!
//! - [`transcribe_pcm`] runs `whisper.cpp` against caller-supplied 16 kHz mono
//!   floating-point PCM. Audio decoding and resampling belong to Cutlass's
//!   native media pipeline and are intentionally not included here.
//! - [`ModelManager`] installs catalogued Whisper models transactionally after
//!   exact size and SHA-256 verification.
//!
//! No model is bundled in the binary and no model download occurs implicitly
//! during transcription. Callers decide where models live and when to install
//! them.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod model;
mod transcribe;
mod transcript;

pub use model::{
    DownloadError, DownloadReader, HttpDownloader, ModelDownloader, ModelIntegrityError,
    ModelManager, ModelManagerError, ModelSpec, ModelStatus, WhisperModel,
};
pub use transcribe::{
    CancellationCheck, NeverCancel, TranscriptionError, TranscriptionOptions,
    TranscriptionOptionsError, transcribe_pcm,
};
pub use transcript::{Transcript, TranscriptSegment, TranscriptWord};

/// The only PCM sample rate accepted by [`transcribe_pcm`].
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;
