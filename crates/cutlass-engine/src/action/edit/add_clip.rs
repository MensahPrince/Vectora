use cutlass_models::{ClipId, MediaId, RationalTime, TimeRange, TrackId};

use crate::action::edit::remove_clip::RemoveClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

#[allow(dead_code)]
pub struct AddClipAction {
    pub track: TrackId,
    pub media: MediaId,
    pub source: TimeRange,
    pub start: RationalTime,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    track: TrackId,
    media: MediaId,
    source: TimeRange,
    start: RationalTime,
) -> Result<(ClipId, Box<dyn EditAction>), EngineError> {
    let id = ctx.project.add_clip(track, media, source, start)?;
    Ok((id, Box::new(RemoveClipAction { clip: id })))
}

impl EditAction for AddClipAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, self.track, self.media, self.source, self.start).map(|(_, inv)| inv)
    }
}
