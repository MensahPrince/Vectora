//! Editor command layer: a closed set of structured, deterministic edits.
//!
//! Every edit is an explicit, serializable value — auditable, replayable, and
//! undo-friendly once wired through `cutlass-engine`.

mod command;

pub use command::{
    ClipId, Command, EditCommand, EditOutcome, MediaId, ProjectCommand, TimeRange, TrackId,
};

use tracing::info;

pub fn init() {
    info!("cutlass-commands ready");
}
