use cutlass_models::{ClipId, ModelError, TimeRange};

use crate::action::edit::restore_clip::RestoreClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

#[allow(dead_code)]
pub struct TrimClipAction {
    pub clip: ClipId,
    pub timeline: TimeRange,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    timeline: TimeRange,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx
        .project
        .clip(clip)
        .cloned()
        .ok_or(ModelError::UnknownClip(clip))?;
    ctx.project.trim_clip(clip, timeline)?;
    Ok(Box::new(RestoreClipAction { clip: before }))
}

impl EditAction for TrimClipAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, self.clip, self.timeline)
    }
}
