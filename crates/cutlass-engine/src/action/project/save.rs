use std::path::PathBuf;

use crate::action::ApplyContext;
use crate::error::EngineError;

pub fn execute(ctx: &mut ApplyContext<'_>, path: PathBuf) -> Result<(), EngineError> {
    ctx.project.save_to_file(&path)?;
    *ctx.project_path = Some(path);
    Ok(())
}
