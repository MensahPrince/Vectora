//! Editing engine: timeline state, playback, and deterministic execution of editor commands.
//!
//! v0 surface (what's here today):
//!
//! - One [`Decoder`] driven on a single worker thread (see `decoder` crate's
//!   `!Sync` contract).
//! - Crossbeam command channel + a latest-wins **scrub slot** so the UI can
//!   spam seeks while dragging the playhead without the worker ever falling
//!   behind on stale targets.
//! - Worker emits ready-to-display [`PreviewFrame`]s (BT.709 YUV → RGBA8 on
//!   the engine thread, never on the UI thread).
//!
//! Future variants of the engine — multiple decoders, a frame cache, playback
//! clock — slot in behind this same [`Engine`] handle.

mod convert;
mod error;
mod playback;

pub use crate::error::EngineError;
pub use crate::playback::{Engine, EngineEvent, EventReceiver, PreviewFrame};
pub use decoder::Rational;
