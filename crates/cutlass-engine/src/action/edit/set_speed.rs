use cutlass_models::{ClipId, ModelError, Rational};

use crate::action::edit::restore_clip::RestoreClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Retime a media clip (CapCut speed, M1). The model re-derives the
/// timeline duration (source ÷ speed) and validates speed positivity,
/// media backing, and neighbor overlap. The inverse is a full-clip
/// restore — speed, reversed, and the re-derived timeline range all roll
/// back in one shot, like the transform and param edits.
pub fn set_speed(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    speed: Rational,
    reversed: bool,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx
        .project
        .clip(clip)
        .cloned()
        .ok_or(ModelError::UnknownClip(clip))?;
    ctx.project.set_clip_speed(clip, speed, reversed)?;
    Ok(Box::new(RestoreClipAction { clip: before }))
}
