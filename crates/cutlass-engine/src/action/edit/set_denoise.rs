use cutlass_models::{ClipId, ModelError};

use crate::action::edit::restore_clip::RestoreClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Toggle noise reduction on a media clip (CapCut "Reduce noise", M8 Phase 5).
/// The model validates media backing; the flag changes no duration, so there is
/// no neighbor check. The inverse is a full-clip restore, like the pitch and
/// audio edits.
pub fn set_denoise(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    denoise: bool,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx
        .project
        .clip(clip)
        .cloned()
        .ok_or(ModelError::UnknownClip(clip))?;
    ctx.project.set_clip_denoise(clip, denoise)?;
    Ok(Box::new(RestoreClipAction { clip: before }))
}
