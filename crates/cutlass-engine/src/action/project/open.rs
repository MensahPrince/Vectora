use std::path::PathBuf;

use crate::action::ApplyContext;
use crate::action::project::load_session;
use crate::error::EngineError;

pub fn execute(ctx: &mut ApplyContext<'_>, path: PathBuf) -> Result<(), EngineError> {
    load_session(ctx, path, true)
}
