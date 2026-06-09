use std::path::PathBuf;

use crate::action::project::load_session;
use crate::action::ApplyContext;
use crate::error::EngineError;

pub fn execute(ctx: &mut ApplyContext<'_>, path: PathBuf) -> Result<(), EngineError> {
    load_session(ctx, path, false)
}
