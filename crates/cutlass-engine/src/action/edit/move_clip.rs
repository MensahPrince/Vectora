use cutlass_models::{ClipId, ModelError, RationalTime, TrackId};

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

pub struct MoveClipAction {
    pub clip: ClipId,
    pub to_track: TrackId,
    pub start: RationalTime,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    to_track: TrackId,
    start: RationalTime,
) -> Result<Box<dyn EditAction>, EngineError> {
    let from_track = ctx
        .project
        .timeline()
        .track_of(clip)
        .ok_or(ModelError::UnknownClip(clip))?;
    let from_start = ctx
        .project
        .clip(clip)
        .ok_or(ModelError::UnknownClip(clip))?
        .start();
    ctx.project.move_clip(clip, to_track, start)?;
    Ok(Box::new(MoveClipAction {
        clip,
        to_track: from_track,
        start: from_start,
    }))
}

impl EditAction for MoveClipAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, self.clip, self.to_track, self.start)
    }
}
