use cutlass_models::{Clip, ClipId, ModelError, TrackId, time_add};

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

pub struct RippleDeleteAction {
    pub clip: ClipId,
}

pub struct RestoreRippleAction {
    pub track: TrackId,
    pub clip: Clip,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
) -> Result<Box<dyn EditAction>, EngineError> {
    let track = ctx
        .project
        .timeline()
        .track_of(clip)
        .ok_or(ModelError::UnknownClip(clip))?;
    let removed = ctx.project.ripple_delete(clip)?;
    Ok(Box::new(RestoreRippleAction {
        track,
        clip: removed,
    }))
}

impl EditAction for RippleDeleteAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, self.clip)
    }
}

impl EditAction for RestoreRippleAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        let gap_start = self.clip.timeline.start;
        let gap = self.clip.timeline.duration;
        let track = ctx
            .project
            .timeline_mut()
            .track_mut(self.track)
            .ok_or(ModelError::UnknownTrack(self.track))?;
        for clip in track.clips_mut() {
            if clip.timeline.start.value >= gap_start.value {
                clip.timeline.start = time_add(&clip.timeline.start, &gap)?;
            }
        }
        let id = self.clip.id;
        ctx.project.timeline_mut().add_clip(self.track, self.clip)?;
        Ok(Box::new(RippleDeleteAction { clip: id }))
    }
}
