use cutlass_models::MediaId;

use crate::action::edit::insert_media::InsertMediaAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

pub struct RemoveMediaAction {
    pub media: MediaId,
}

impl EditAction for RemoveMediaAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let media = ctx.project.remove_media(self.media)?;
        Ok(Box::new(InsertMediaAction { media }))
    }
}
