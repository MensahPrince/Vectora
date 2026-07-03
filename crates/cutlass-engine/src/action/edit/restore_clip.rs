use cutlass_models::{Clip, ModelError};

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Swap a clip back to a captured snapshot (trim undo/redo).
pub struct RestoreClipAction {
    pub clip: Clip,
}

impl EditAction for RestoreClipAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let id = self.clip.id;
        let current = ctx
            .project
            .clip(id)
            .cloned()
            .ok_or(ModelError::UnknownClip(id))?;
        *ctx.project
            .timeline_mut()
            .clip_mut(id)
            .ok_or(ModelError::UnknownClip(id))? = self.clip;
        Ok(Box::new(RestoreClipAction { clip: current }))
    }
}
