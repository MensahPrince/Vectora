use std::path::{Path, PathBuf};

use cutlass_models::MediaId;

use crate::action::edit::remove_media::RemoveMediaAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;
use crate::import::import_media;

pub struct ImportAction {
    pub path: PathBuf,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    path: &Path,
) -> Result<(MediaId, Box<dyn EditAction>), EngineError> {
    let media = import_media(path, ctx.cache)?;
    let id = ctx.project.add_media(media);
    Ok((id, Box::new(RemoveMediaAction { media: id })))
}

impl EditAction for ImportAction {
    fn apply(self: Box<Self>, ctx: &mut ApplyContext<'_>) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, &self.path).map(|(_, inv)| inv)
    }
}
