//! Decode scheduling and worker lifecycle between [`decoder`] and UI / renderer / tests.
//!
//! # Docs
//! - Behaviour and boundaries: `docs/engine/research.md`
//! - MVP phases and roadmap maintenance notes: `docs/engine/roadmap.md`
//!
//! # Overview
//! [`Engine`] submits work to a dedicated thread that owns [`decoder::Decoder`] state; results arrive on [`EventReceiver`].
//! Channel capacities are [`CMD_CHANNEL_CAPACITY`] / [`EVENT_CHANNEL_CAPACITY`] / [`SCRUB_SIGNAL_CAPACITY`].

mod command;
mod engine;
mod error;
mod event;
mod ids;
mod worker;

pub use command::EngineCommand;
pub use decoder::{
    DecodeOutcome, DecodedVideoFrame, DecoderError, PixelFormat, Rational, SourceInfo,
};
pub use engine::{
    Engine, EventReceiver, CMD_CHANNEL_CAPACITY, EVENT_CHANNEL_CAPACITY, SCRUB_SIGNAL_CAPACITY,
};
pub use error::EngineError;
pub use event::EngineEvent;
pub use ids::{RequestId, SourceId};
