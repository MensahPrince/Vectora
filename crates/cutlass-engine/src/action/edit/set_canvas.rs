//! Canvas settings edit (M1): aspect preset + background color, swapped as
//! one undoable unit.

use cutlass_models::{CanvasAspect, CanvasSettings};

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Set the project canvas; the inverse restores the prior settings.
pub fn set_canvas(
    ctx: &mut ApplyContext<'_>,
    aspect: CanvasAspect,
    background: [u8; 3],
) -> Result<Box<dyn EditAction>, EngineError> {
    let timeline = ctx.project.timeline_mut();
    let before = timeline.canvas();
    timeline.set_canvas(CanvasSettings { aspect, background });
    Ok(Box::new(RestoreCanvasAction { settings: before }))
}

/// Swap the canvas back to a captured snapshot (set-canvas undo/redo).
struct RestoreCanvasAction {
    settings: CanvasSettings,
}

impl EditAction for RestoreCanvasAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let timeline = ctx.project.timeline_mut();
        let current = timeline.canvas();
        timeline.set_canvas(self.settings);
        Ok(Box::new(RestoreCanvasAction { settings: current }))
    }
}
