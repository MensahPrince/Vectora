//! Project / timeline model: tracks, clips, commands, undo, JSON persistence.
//!
//! Pure data + logic — no I/O, no async, no engine/renderer coupling. See `docs/timeline/research.md`.

mod command;
mod commands;
mod error;
mod history;
mod ids;
mod mapping;
mod model;
mod serialize;
mod time;

pub use command::Command;
pub use commands::{
    AddClip, AddSource, AddTrack, MoveClip, RemoveClip, RemoveSource, RemoveTrack, SetSourceProbed,
    TrimClipIn, TrimClipOut,
};
pub use decoder::Rational;
pub use error::TimelineError;
pub use history::History;
pub use ids::{ClipId, MediaSourceId, ProjectId, TrackId};
pub use mapping::{active_clip_on_track, ActiveClip};
pub use model::{
    Clip, MediaSource, Project, ProjectSettings, ProbedInfo, Track, TrackKind,
    CURRENT_SCHEMA_VERSION,
};
pub use serialize::{deserialize_project, serialize_project};
