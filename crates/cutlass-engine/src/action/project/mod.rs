//! Session-level project commands (not undoable except import's inverse).

use std::path::PathBuf;

use cutlass_cache::{CacheSpec, FrameCache, SourceFingerprint};
use cutlass_models::Project;

use crate::action::ApplyContext;
use crate::error::EngineError;

pub mod import;
pub mod load;
pub mod open;
pub mod save;

pub(crate) fn load_session(
    ctx: &mut ApplyContext<'_>,
    path: PathBuf,
    strict: bool,
) -> Result<(), EngineError> {
    let loaded = Project::load_from_file(&path)?;
    relink_media_cache(ctx.cache, &loaded, strict)?;
    *ctx.project = loaded;
    *ctx.project_path = Some(path);
    ctx.history.clear();
    Ok(())
}

fn relink_media_cache(
    cache: &FrameCache,
    project: &Project,
    strict: bool,
) -> Result<(), EngineError> {
    for media in project.media_iter() {
        if !media.path().exists() {
            if strict {
                return Err(EngineError::MissingMedia(
                    media.path().display().to_string(),
                ));
            }
            continue;
        }
        let fingerprint = SourceFingerprint::from_path(media.path())?;
        let spec = CacheSpec {
            width: media.width,
            height: media.height,
            pixfmt: "yuv420p".into(),
        };
        cache
            .register_source(fingerprint, spec)
            .map_err(EngineError::from)?;
    }
    Ok(())
}
