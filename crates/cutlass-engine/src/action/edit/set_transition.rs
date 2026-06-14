use cutlass_models::ClipId;

use crate::action::edit::restore_transitions::RestoreTransitionsAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

// Transition edits (M4). All three share a transitions-snapshot inverse: the
// per-track sets are tiny, and a snapshot rolls back add / remove /
// duration-edit unconditionally — mirroring the clip-snapshot shape used by
// the effect edits.

pub fn add_transition(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    transition_id: &str,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx.project.transitions_snapshot();
    ctx.project.add_transition(clip, transition_id)?;
    Ok(Box::new(RestoreTransitionsAction { snapshot: before }))
}

pub fn remove_transition(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx.project.transitions_snapshot();
    ctx.project.remove_transition(clip)?;
    Ok(Box::new(RestoreTransitionsAction { snapshot: before }))
}

pub fn set_transition(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    duration: i64,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx.project.transitions_snapshot();
    ctx.project.set_transition_duration(clip, duration)?;
    Ok(Box::new(RestoreTransitionsAction { snapshot: before }))
}
