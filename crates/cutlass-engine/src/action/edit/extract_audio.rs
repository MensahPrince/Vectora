use cutlass_models::{
    Clip, ClipId, ClipSource, LinkId, MediaKind, ModelError, Project, Track, TrackId, TrackKind,
};

use crate::action::edit::add_track::InsertTrackAction;
use crate::action::edit::insert_clip::InsertClipAction;
use crate::action::edit::link_clips::SetClipLinksAction;
use crate::action::{ApplyContext, CompoundAction, EditAction};
use crate::error::EngineError;

enum DestinationPlan {
    Existing(TrackId),
    Create { track: Track, order_index: usize },
}

struct ExtractPlan {
    source: ClipId,
    destination: DestinationPlan,
    companion: Clip,
    link: LinkId,
}

/// Extract a video clip's embedded sound as one atomic engine edit.
///
/// The full plan is validated and snapshotted before the first project
/// mutation. Internal actions are then applied directly so the command records
/// exactly one compound inverse even when it also creates an audio lane.
pub fn execute(
    ctx: &mut ApplyContext<'_>,
    clip: ClipId,
    to_track: Option<TrackId>,
) -> Result<(ClipId, Box<dyn EditAction>), EngineError> {
    let plan = build_plan(ctx.project, clip, to_track)?;
    let companion_id = plan.companion.id;
    let destination_id = match &plan.destination {
        DestinationPlan::Existing(id) => *id,
        DestinationPlan::Create { track, .. } => track.id,
    };

    let mut actions: Vec<Box<dyn EditAction>> = Vec::with_capacity(3);
    if let DestinationPlan::Create { track, order_index } = plan.destination {
        actions.push(Box::new(InsertTrackAction { track, order_index }));
    }
    actions.push(Box::new(InsertClipAction {
        track: destination_id,
        clip: plan.companion,
    }));
    actions.push(Box::new(SetClipLinksAction {
        links: vec![
            (plan.source, Some(plan.link)),
            (companion_id, Some(plan.link)),
        ],
    }));

    let inverse = apply_atomically(ctx, actions)?;
    Ok((companion_id, inverse))
}

fn build_plan(
    project: &Project,
    clip_id: ClipId,
    to_track: Option<TrackId>,
) -> Result<ExtractPlan, ModelError> {
    let timeline = project.timeline();
    let source_track_id = timeline
        .track_of(clip_id)
        .ok_or(ModelError::UnknownClip(clip_id))?;
    let source_track = timeline
        .track(source_track_id)
        .ok_or(ModelError::UnknownTrack(source_track_id))?;
    let source = timeline
        .clip(clip_id)
        .ok_or(ModelError::UnknownClip(clip_id))?;
    let ClipSource::Media { media, .. } = &source.content else {
        return Err(ModelError::InvalidParam(
            "extract audio requires a media-backed clip".into(),
        ));
    };
    if source_track.kind != TrackKind::Video {
        return Err(ModelError::InvalidParam(format!(
            "extract audio requires a clip on a video track (clip {clip_id} is on {source_track_id})"
        )));
    }
    if source_track.locked {
        return Err(ModelError::InvalidParam(format!(
            "source video track {source_track_id} is locked"
        )));
    }
    let media_source = project
        .media(*media)
        .ok_or(ModelError::UnknownMedia(*media))?;
    if media_source.kind() != MediaKind::Video {
        return Err(ModelError::InvalidParam(format!(
            "clip {clip_id} does not reference video media"
        )));
    }
    if !media_source.has_audio {
        return Err(ModelError::InvalidParam(format!(
            "clip {clip_id} has no audio stream to extract"
        )));
    }
    if timeline.detached_to_audio_lane(clip_id) {
        return Err(ModelError::InvalidParam(format!(
            "clip {clip_id} already has extracted audio"
        )));
    }
    if let Some(link) = source.link {
        for track in timeline.tracks_ordered() {
            if let Some(other) = track
                .clips()
                .find(|candidate| candidate.id != clip_id && candidate.link == Some(link))
            {
                return Err(ModelError::InvalidParam(format!(
                    "clip {clip_id} is linked to {other_id}; unlink the group before extracting audio",
                    other_id = other.id
                )));
            }
        }
    }

    let destination = match to_track {
        Some(track_id) => {
            let track = timeline
                .track(track_id)
                .ok_or(ModelError::UnknownTrack(track_id))?;
            if track.kind != TrackKind::Audio {
                return Err(ModelError::IncompatibleTrackKind {
                    track: track_id,
                    kind: track.kind,
                });
            }
            if track.locked {
                return Err(ModelError::InvalidParam(format!(
                    "target audio track {track_id} is locked"
                )));
            }
            if track.has_overlap(source.timeline, None)? {
                return Err(ModelError::Overlap(track_id));
            }
            DestinationPlan::Existing(track_id)
        }
        None => {
            let mut available = None;
            let mut audio_count = 0usize;
            for track in timeline.tracks_ordered() {
                if track.kind != TrackKind::Audio {
                    continue;
                }
                audio_count += 1;
                if available.is_none()
                    && !track.locked
                    && !track.has_overlap(source.timeline, None)?
                {
                    available = Some(track.id);
                }
            }
            match available {
                Some(track_id) => DestinationPlan::Existing(track_id),
                None => {
                    let track = loop {
                        let candidate =
                            Track::new(TrackKind::Audio, format!("A{}", audio_count + 1));
                        if timeline.track(candidate.id).is_none() {
                            break candidate;
                        }
                    };
                    DestinationPlan::Create {
                        track,
                        order_index: timeline.order().len(),
                    }
                }
            }
        }
    };

    // This helper allocates the companion only after every rejection-prone
    // source and destination check above has passed.
    let companion = loop {
        let candidate = source.extracted_audio_companion()?;
        if timeline.clip(candidate.id).is_none() {
            break candidate;
        }
    };
    let link = loop {
        let candidate = LinkId::next();
        let in_use = timeline
            .tracks_ordered()
            .flat_map(|track| track.clips())
            .any(|clip| clip.link == Some(candidate));
        if !in_use {
            break candidate;
        }
    };
    Ok(ExtractPlan {
        source: clip_id,
        destination,
        companion,
        link,
    })
}

fn apply_atomically(
    ctx: &mut ApplyContext<'_>,
    actions: Vec<Box<dyn EditAction>>,
) -> Result<Box<dyn EditAction>, EngineError> {
    let mut inverses = Vec::with_capacity(actions.len());
    for action in actions {
        match action.apply(ctx) {
            Ok(inverse) => inverses.push(inverse),
            Err(error) => {
                while let Some(inverse) = inverses.pop() {
                    if let Err(rollback_error) = inverse.apply(ctx) {
                        tracing::error!(
                            "extract-audio rollback failed after {error}: {rollback_error}"
                        );
                        break;
                    }
                }
                return Err(error);
            }
        }
    }
    Ok(Box::new(CompoundAction { actions: inverses }))
}
