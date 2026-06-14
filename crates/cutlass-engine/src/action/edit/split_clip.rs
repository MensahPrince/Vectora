use cutlass_models::{Clip, ClipId, ModelError, RationalTime};

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

pub struct SplitClipAction {
    pub clip: ClipId,
    pub at: RationalTime,
}

pub struct MergeSplitAction {
    pub left_id: ClipId,
    pub restored: Clip,
    pub right_id: ClipId,
    pub at: RationalTime,
}

pub fn execute(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    at: RationalTime,
) -> Result<(ClipId, Box<dyn EditAction>), EngineError> {
    let restored = ctx
        .project
        .clip(clip)
        .cloned()
        .ok_or(ModelError::UnknownClip(clip))?;
    let right_id = ctx.project.split_clip(clip, at)?;
    Ok((
        right_id,
        Box::new(MergeSplitAction {
            left_id: clip,
            restored,
            right_id,
            at,
        }),
    ))
}

impl EditAction for SplitClipAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        execute(ctx, self.clip, self.at).map(|(_, inv)| inv)
    }
}

impl EditAction for MergeSplitAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        ctx.project
            .remove_clip(self.right_id)
            .ok_or(ModelError::UnknownClip(self.right_id))?;
        *ctx.project
            .timeline_mut()
            .clip_mut(self.left_id)
            .ok_or(ModelError::UnknownClip(self.left_id))? = self.restored;
        Ok(Box::new(SplitClipAction {
            clip: self.left_id,
            at: self.at,
        }))
    }
}
