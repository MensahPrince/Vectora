use cutlass_models::{ClipId, ModelError, RationalTime};

use crate::action::edit::restore_clip::RestoreClipAction;
use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Set a media clip's audio mix (CapCut volume + fades). `volume` is `Some`
/// for the basic flat-level slider (overwrites the gain, flattening an M8
/// envelope) or `None` to keep the gain and change only the fades. The model
/// validates the gain range, media backing, and fade durations. The inverse
/// is a full-clip restore — gain and both fades roll back in one shot, like
/// the speed and transform edits.
pub fn set_audio(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    volume: Option<f32>,
    fade_in: RationalTime,
    fade_out: RationalTime,
) -> Result<Box<dyn EditAction>, EngineError> {
    let before = ctx
        .project
        .clip(clip)
        .cloned()
        .ok_or(ModelError::UnknownClip(clip))?;
    ctx.project
        .set_clip_audio(clip, volume, fade_in, fade_out)?;
    Ok(Box::new(RestoreClipAction { clip: before }))
}
