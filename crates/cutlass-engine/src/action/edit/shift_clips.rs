use cutlass_models::{RationalTime, TrackId};

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Ripple primitive: shift every clip on `track` with start ≥ `from` by
/// `delta` ticks. Self-inverse with negated delta; when nothing shifted, the
/// inverse re-selects the same (empty) set, so it stays a no-op.
pub struct ShiftClipsAction {
    pub track: TrackId,
    pub from: RationalTime,
    pub delta: RationalTime,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    track: TrackId,
    from: RationalTime,
    delta: RationalTime,
) -> Result<Box<dyn EditAction>, EngineError> {
    let first_new_start = ctx.project.shift_clips(track, from, delta)?;
    // The inverse must select exactly the clips that moved. Their new starts
    // all sit at/after the first shifted clip's new start, while unshifted
    // clips end at/before it (no-overlap invariant) — so that boundary is
    // exact. When nothing moved, the original `from` re-selects nothing.
    Ok(Box::new(ShiftClipsAction {
        track,
        from: first_new_start.unwrap_or(from),
        delta: RationalTime::new(-delta.value, delta.rate),
    }))
}

impl EditAction for ShiftClipsAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, self.track, self.from, self.delta)
    }
}
