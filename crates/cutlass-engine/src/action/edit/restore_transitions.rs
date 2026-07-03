use cutlass_models::{TrackId, Transition};

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Swap every track's transition set back to a captured snapshot. Used both as
/// the direct inverse of transition edits and, compounded with a structural
/// edit's inverse, to restore junctions pruned when an edit broke an abutment.
pub struct RestoreTransitionsAction {
    pub snapshot: Vec<(TrackId, Vec<Transition>)>,
}

impl EditAction for RestoreTransitionsAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let current = ctx.project.transitions_snapshot();
        ctx.project.restore_transitions(self.snapshot);
        Ok(Box::new(RestoreTransitionsAction { snapshot: current }))
    }
}
