use cutlass_models::{ClipId, ModelError};

use crate::action::edit::restore_clip::RestoreClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Toggle pitch preservation on a retimed media clip (CapCut's "pitch"
/// switch, M8 Phase 3). The model validates media backing; the flag changes
/// no duration, so there is no neighbor check. The inverse is a full-clip
/// restore, like the speed and param edits.
pub fn set_pitch(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    preserve_pitch: bool,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx
        .project
        .clip(clip)
        .cloned()
        .ok_or(ModelError::UnknownClip(clip))?;
    ctx.project.set_clip_pitch(clip, preserve_pitch)?;
    Ok(Box::new(RestoreClipAction { clip: before }))
}
