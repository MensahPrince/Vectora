//! Video demux + decode: passive [`Decoder`] for use on an engine-owned worker thread.
//!
//! # Seek semantics
//! Use [`Decoder::seek_scrub`] for interactive scrub (keyframe snap, first picture only) and
//! [`Decoder::seek_exact`] when the displayed frame must match a timeline time — see repository
//! `docs/decoder/research.md`.
//!
//! # Time
//! [`Rational`] is the source of truth for PTS and duration in the public API (seconds as `num/den`).
//!
//! # Threading
//! `Decoder` must be driven from **one** thread at a time; see `docs/decoder/research.md` (passive
//! decoder + engine-owned worker + channels).

mod decoder;
mod error;
mod frame;
mod outcome;
mod pixel;
mod probe;
mod source;
mod time;

pub use crate::decoder::{Decoder, ffmpeg_version};
pub use error::DecoderError;
pub use frame::{CpuFrame, DecodedVideoFrame, FrameData, Plane};
pub use outcome::DecodeOutcome;
pub use pixel::PixelFormat;
pub use probe::{ProbedAudio, ProbedKind, ProbedSource, ProbedVideo, probe};
pub use source::SourceInfo;
pub use time::Rational;

#[cfg(test)]
mod rational_ext_tests;
