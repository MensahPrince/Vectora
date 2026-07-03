use cutlass_models::{ClipId, ModelError, Param};

use crate::action::edit::restore_clip::RestoreClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Set (or clear) a media clip's playback-rate ramp (CapCut speed curves,
/// M2). The model validates the curve, re-derives the timeline duration from
/// the curve's average, and checks neighbor overlap. The inverse is a
/// full-clip restore — the curve and the re-derived timeline range roll back
/// in one shot, like the constant-speed and param edits.
pub fn set_speed_curve(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    curve: Option<Param<f32>>,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx
        .project
        .clip(clip)
        .cloned()
        .ok_or(ModelError::UnknownClip(clip))?;
    ctx.project.set_clip_speed_curve(clip, curve)?;
    Ok(Box::new(RestoreClipAction { clip: before }))
}
