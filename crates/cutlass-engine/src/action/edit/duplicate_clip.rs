use cutlass_models::{ClipId, RationalTime, TrackId};

use crate::action::edit::remove_clip::RemoveClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Deep-copy one clip to an explicit destination and return its undo action.
pub fn execute(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    to_track: TrackId,
    start: RationalTime,
) -> Result<(ClipId, Box<dyn EditAction>), EngineError> {
    let duplicate = ctx.project.duplicate_clip(clip, to_track, start)?;
    Ok((duplicate, Box::new(RemoveClipAction { clip: duplicate })))
}
