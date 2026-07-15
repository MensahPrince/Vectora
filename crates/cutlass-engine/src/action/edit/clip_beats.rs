//! Pure beat-marker edits.
//!
//! Beat analysis is asynchronous decode/DSP work and does not belong in pure
//! command dispatch. Clearing an existing grid is pure: the inverse stores
//! only the prior beat list, so undo cannot replace unrelated clip fields.

use cutlass_models::ClipId;

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Clear a media clip's beat markers and return an inverse restoring only them.
///
/// An already-empty grid still produces an inverse. At the command layer that
/// makes `ClearBeats` one deterministic, reversible history step whose
/// undo/redo both leave the empty grid unchanged.
pub fn clear(ctx: &mut ApplyContext<'_>, clip: ClipId) -> Result<Box<dyn EditAction>, EngineError> {
    let beats = ctx.project.set_clip_beats(clip, Vec::new())?;
    Ok(Box::new(SetClipBeatsAction { clip, beats }))
}

/// Swap one clip's beat grid with a captured value.
///
/// This is deliberately narrower than a whole-clip snapshot: actions recorded
/// after this one may evolve other clip properties without this inverse owning
/// or overwriting them.
struct SetClipBeatsAction {
    clip: ClipId,
    beats: Vec<i64>,
}

impl EditAction for SetClipBeatsAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let Self { clip, beats } = *self;
        let beats = ctx.project.set_clip_beats(clip, beats)?;
        Ok(Box::new(Self { clip, beats }))
    }
}
