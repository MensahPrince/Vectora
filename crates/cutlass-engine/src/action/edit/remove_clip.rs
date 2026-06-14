use cutlass_models::{ClipId, ModelError};

use crate::action::edit::insert_clip::InsertClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

pub struct RemoveClipAction {
    pub clip: ClipId,
}

impl EditAction for RemoveClipAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let track = ctx
            .project
            .timeline()
            .track_of(self.clip)
            .ok_or(ModelError::UnknownClip(self.clip))?;
        let clip = ctx
            .project
            .remove_clip(self.clip)
            .ok_or(ModelError::UnknownClip(self.clip))?;
        Ok(Box::new(InsertClipAction { track, clip }))
    }
}
