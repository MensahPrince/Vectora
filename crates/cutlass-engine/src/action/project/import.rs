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
) -> Result<(MediaId, Option<Box<dyn EditAction>>), EngineError> {
    if let Some(existing) = ctx.project.find_media_by_path(path) {
        return Ok((existing, None));
    }

    let path = path.canonicalize().map_err(EngineError::Io)?;
    let media = import_media(&path, ctx.cache)?;
    let id = ctx.project.add_media(media);
    Ok((id, Some(Box::new(RemoveMediaAction { media: id }))))
}

impl EditAction for ImportAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let (_, inverse) = execute(ctx, &self.path)?;
        inverse.ok_or_else(|| {
            EngineError::Import(format!(
                "cannot import duplicate media: {}",
                self.path.display()
            ))
        })
    }
}
