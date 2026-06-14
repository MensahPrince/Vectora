use cutlass_models::{ClipId, Generator, ModelError};

use crate::action::edit::restore_clip::RestoreClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Replace a generated clip's content (title text, shape color, …). The
/// inverse is a full-clip restore, so it oscillates like trim's
/// `RestoreClipAction`.
#[allow(dead_code)]
pub struct SetGeneratorAction {
    pub clip: ClipId,
    pub generator: Generator,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    generator: Generator,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx
        .project
        .clip(clip)
        .cloned()
        .ok_or(ModelError::UnknownClip(clip))?;
    ctx.project.set_generator(clip, generator)?;
    Ok(Box::new(RestoreClipAction { clip: before }))
}

impl EditAction for SetGeneratorAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, self.clip, self.generator)
    }
}
