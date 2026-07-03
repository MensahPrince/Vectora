use cutlass_models::{ClipId, Generator, TimeRange, TrackId};

use crate::action::edit::remove_clip::RemoveClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

#[allow(dead_code)]
pub struct AddGeneratedAction {
    pub track: TrackId,
    pub generator: Generator,
    pub timeline: TimeRange,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    track: TrackId,
    generator: Generator,
    timeline: TimeRange,
) -> Result<(ClipId, Box<dyn EditAction>), EngineError> {
    let id = ctx.project.add_generated(track, generator, timeline)?;
    Ok((id, Box::new(RemoveClipAction { clip: id })))
}

impl EditAction for AddGeneratedAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, self.track, self.generator, self.timeline).map(|(_, inv)| inv)
    }
}
