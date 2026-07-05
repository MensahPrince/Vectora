//! Project rename: swap the project's display name as one undoable unit.

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Set the project's display name; the inverse restores the prior name.
pub fn set_project_name(
    ctx: &mut ApplyContext<'_>,
    name: String,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = std::mem::replace(&mut ctx.project.name, name);
    Ok(Box::new(RestoreProjectNameAction { name: before }))
}

/// Swap the project name back to a captured value (rename undo/redo).
struct RestoreProjectNameAction {
    name: String,
}

impl EditAction for RestoreProjectNameAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let current = std::mem::replace(&mut ctx.project.name, self.name);
        Ok(Box::new(RestoreProjectNameAction { name: current }))
    }
}
