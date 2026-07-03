use cutlass_models::{ClipId, MediaId, RationalTime, TimeRange, TrackId, resample};

use crate::action::edit::{add_clip, shift_clips};
use crate::action::{ApplyContext, CompoundAction, EditAction};
use crate::error::EngineError;

/// CapCut main-track insert: shift every clip starting at/after `at` right by
/// the new clip's timeline duration, then place the clip in the opened hole.
/// Atomic — if the placement is rejected, the shift is reverted before the
/// error propagates. The inverse removes the clip and closes the hole again.
pub fn execute(
    ctx: &mut ApplyContext<'_>,
    track: TrackId,
    media: MediaId,
    source: TimeRange,
    at: RationalTime,
) -> Result<(ClipId, Box<dyn EditAction>), EngineError> {
    let tl_rate = ctx.project.timeline().frame_rate;
    // Mirror Project::add_clip's source→timeline resampling so the hole is
    // exactly as wide as the clip the engine will place.
    let duration_ticks = resample(source.duration, tl_rate).value.max(1);
    let delta = RationalTime::new(duration_ticks, tl_rate);

    let shift_inverse = shift_clips::execute(ctx, track, at, delta)?;
    match add_clip::execute(ctx, track, media, source, at) {
        Ok((id, add_inverse)) => Ok((
            id,
            Box::new(CompoundAction {
                actions: vec![shift_inverse, add_inverse],
            }),
        )),
        Err(err) => {
            // Re-close the hole so a rejected placement leaves no trace. The
            // shift inverse restores positions that were valid a moment ago;
            // surface the original placement error regardless.
            if shift_inverse.apply(ctx).is_err() {
                tracing::error!(%track, "ripple-insert rollback failed; hole left open");
            }
            Err(err)
        }
    }
}
