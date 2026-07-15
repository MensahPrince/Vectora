use std::collections::BTreeSet;

use cutlass_models::{ClipId, LinkId, ModelError};

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

/// The undoable unit for clip linkage: assign each clip the paired link
/// value, returning an inverse that restores what was there before. Linking
/// a set records every member's prior link (so undo re-links a clip that was
/// already in another group); the inverse oscillates like any snapshot swap.
pub struct SetClipLinksAction {
    pub links: Vec<(ClipId, Option<LinkId>)>,
}

/// Put `clips` into one freshly allocated link group.
pub fn execute(
    ctx: &mut ApplyContext<'_>,
    clips: &[ClipId],
) -> Result<Box<dyn EditAction>, EngineError> {
    if clips.is_empty() {
        return Err(EngineError::from(ModelError::InvalidRange));
    }
    let link = LinkId::next();
    Box::new(SetClipLinksAction {
        links: clips.iter().map(|&clip| (clip, Some(link))).collect(),
    })
    .apply(ctx)
}

/// Dissolve every link group touched by `clips`.
///
/// Inputs are validated before mutation. Unlinked inputs are ignored when at
/// least one linked group is touched; an entirely unlinked input set is
/// rejected. Every live timeline member of each touched group is cleared in
/// deterministic track/clip order.
pub fn unlink(
    ctx: &mut ApplyContext<'_>,
    clips: &[ClipId],
) -> Result<Box<dyn EditAction>, EngineError> {
    if clips.is_empty() {
        return Err(EngineError::from(ModelError::InvalidRange));
    }

    let mut touched = BTreeSet::new();
    for &clip_id in clips {
        let clip = ctx
            .project
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if let Some(link) = clip.link {
            touched.insert(link);
        }
    }
    if touched.is_empty() {
        return Err(EngineError::from(ModelError::InvalidRange));
    }

    let mut links = Vec::new();
    for track in ctx.project.timeline().tracks_ordered() {
        for clip in track.clips_ordered() {
            if clip.link.is_some_and(|link| touched.contains(&link)) {
                links.push((clip.id, None));
            }
        }
    }

    Box::new(SetClipLinksAction { links }).apply(ctx)
}

impl EditAction for SetClipLinksAction {
    fn apply(
        self: Box<Self>,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<Box<dyn EditAction>, EngineError> {
        // Validate the whole set before mutating anything, so a missing clip
        // can't leave the group half-applied.
        for (clip, _) in &self.links {
            if ctx.project.clip(*clip).is_none() {
                return Err(EngineError::from(ModelError::UnknownClip(*clip)));
            }
        }
        let mut previous = Vec::with_capacity(self.links.len());
        for (clip_id, link) in self.links {
            let clip = ctx
                .project
                .timeline_mut()
                .clip_mut(clip_id)
                .ok_or(ModelError::UnknownClip(clip_id))?;
            previous.push((clip_id, clip.link));
            clip.link = link;
        }
        Ok(Box::new(SetClipLinksAction { links: previous }))
    }
}
