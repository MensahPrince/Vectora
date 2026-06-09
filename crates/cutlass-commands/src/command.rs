//! Structured editor commands.
//!
//! UI gestures and the AI agent both emit these values; the engine applies them
//! against project/timeline state with undo/redo.

use std::path::PathBuf;

/// Placeholder IDs until `cutlass-models` exports the real newtypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClipId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MediaId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(pub u64);

/// Inclusive timeline or source frame range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: i64,
    pub end: i64,
}

/// A project-level action (media pool, not timeline placement).
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectCommand {
    /// Register a file in the media pool.
    Import { path: PathBuf },
}

/// A single structured edit against the timeline.
#[derive(Debug, Clone, PartialEq)]
pub enum EditCommand {
    /// Place a trimmed range of imported media on a track.
    AddClip {
        track: TrackId,
        media: MediaId,
        source: TimeRange,
        start: i64,
    },
    /// Place a generated clip (text, solid, shape, …) on a track.
    AddGenerated {
        track: TrackId,
        /// Generator payload TBD in `cutlass-models`.
        generator: (),
        timeline: TimeRange,
    },
    /// Split a clip at a timeline frame into two abutting clips.
    SplitClip { clip: ClipId, at: i64 },
    /// Re-place / trim a clip to occupy `timeline`.
    TrimClip { clip: ClipId, timeline: TimeRange },
    /// Move a clip to `to_track` starting at `start`, keeping its duration.
    MoveClip {
        clip: ClipId,
        to_track: TrackId,
        start: i64,
    },
    /// Remove a clip, leaving a gap where it sat.
    RemoveClip { clip: ClipId },
    /// Remove a clip and slide later clips on its track left to close the gap.
    RippleDelete { clip: ClipId },
}

/// Top-level command surface: media registration or a timeline edit.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Project(ProjectCommand),
    Edit(EditCommand),
}

/// What an applied edit produced, for callers to act on (e.g. select the new clip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    Created(ClipId),
    Updated(ClipId),
    Removed(ClipId),
}
