//! Validation + lowering: wire commands → engine commands, against a live
//! project snapshot.
//!
//! This is the guardrail chokepoint. Every rejection carries a
//! model-readable message (including the ids that *do* exist) so the agent
//! loop can feed it straight back as a tool result and let the model correct
//! course. Checks here are for good error messages and the whitelist; the
//! engine remains the authority (overlaps, source bounds, rate math) and
//! re-validates everything on apply.

use std::collections::HashSet;

use cutlass_commands::{Command, EditCommand};
use cutlass_models::{
    AnimationRef, AnimationSlot, AudioRole, CanvasAspect, ChromaKey, Clip, ClipId, ClipParam,
    ClipTransform, CropRect, Easing, Filter, Generator, Marker, MarkerColor, MarkerId, Mask,
    MaskKind, MediaId, Param, ParamValue, Project, Rational, RationalTime, StabilizeLevel,
    TimeRange, TrackId, TrackKind, animation_spec, filter_catalog, filter_spec,
};

use crate::wire::{
    MAX_MULTI_CLIP_REFS, WireAnimationSlot, WireAudioRole, WireCanvasAspect, WireChromaKey,
    WireClipParam, WireCommand, WireEasing, WireGenerator, WireMarkerColor, WireMask, WireMaskKind,
    WireShape, WireStabilizeLevel, WireTrackKind,
};

/// A wire command the project as it stands cannot accept. The message is
/// written for the model: it names the problem and the valid alternatives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub message: String,
}

impl Rejection {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Rejection {}

/// Validate `command` against `project` and lower it to an engine command.
pub fn validate(command: &WireCommand, project: &Project) -> Result<Command, Rejection> {
    let edit = match command {
        WireCommand::AddTrack(args) => EditCommand::AddTrack {
            kind: track_kind(args.kind),
            name: args.name.clone(),
            index: args.index.map(|i| i as usize),
            pinned: false,
        },
        WireCommand::AddClip(args) => {
            let track = track_ref(project, args.track)?;
            let media = media_ref(project, args.media)?;
            require_media_lane(track.kind, args.track)?;
            let (source, start) = media_placement(
                project,
                media,
                args.source_start,
                args.source_duration,
                args.start,
                "start",
            )?;
            EditCommand::AddClip {
                track: track.id,
                media: media.id,
                source,
                start,
            }
        }
        WireCommand::ExtractAudio(args) => {
            let clip = clip_ref(project, args.clip)?;
            let timeline = project.timeline();
            let source_track_id = timeline.track_of(clip.id).ok_or_else(|| {
                Rejection::new(format!("clip {} is not on the timeline", args.clip))
            })?;
            let source_track = timeline.track(source_track_id).ok_or_else(|| {
                Rejection::new(format!(
                    "clip {} refers to missing track {}",
                    args.clip,
                    source_track_id.raw()
                ))
            })?;
            let cutlass_models::ClipSource::Media { media, .. } = &clip.content else {
                return Err(Rejection::new(format!(
                    "clip {} is generated; extract_audio requires a media-backed video clip",
                    args.clip
                )));
            };
            if source_track.kind != TrackKind::Video {
                return Err(Rejection::new(format!(
                    "clip {} is on a {} track; extract_audio requires a video-track clip",
                    args.clip,
                    kind_name(source_track.kind)
                )));
            }
            if source_track.locked {
                return Err(Rejection::new(format!(
                    "video track {} is locked; unlock it before extracting clip {}'s audio",
                    source_track_id.raw(),
                    args.clip
                )));
            }
            let media = media_ref(project, media.raw())?;
            if media.kind() != cutlass_models::MediaKind::Video {
                return Err(Rejection::new(format!(
                    "clip {} does not reference video media; extract_audio requires a video file",
                    args.clip
                )));
            }
            if !media.has_audio {
                return Err(Rejection::new(format!(
                    "clip {} uses media {} with no audio stream to extract",
                    args.clip,
                    media.id.raw()
                )));
            }
            if timeline.detached_to_audio_lane(clip.id) {
                return Err(Rejection::new(format!(
                    "clip {} already has extracted audio; use its linked audio-lane companion",
                    args.clip
                )));
            }
            if let Some(link) = clip.link {
                let other_members: Vec<u64> = timeline
                    .tracks_ordered()
                    .flat_map(|track| track.clips())
                    .filter(|candidate| candidate.id != clip.id && candidate.link == Some(link))
                    .map(|candidate| candidate.id.raw())
                    .collect();
                if !other_members.is_empty() {
                    return Err(Rejection::new(format!(
                        "clip {} is already linked to clips {}; call unlink_clips first, then \
                         retry extract_audio",
                        args.clip,
                        list_ids(other_members)
                    )));
                }
            }

            let target = track_ref(project, args.track)?;
            if target.kind != TrackKind::Audio {
                return Err(Rejection::new(format!(
                    "track {} is a {} track; extract_audio needs an audio track",
                    args.track,
                    kind_name(target.kind)
                )));
            }
            if target.locked {
                return Err(Rejection::new(format!(
                    "audio track {} is locked; unlock it or choose another audio track",
                    args.track
                )));
            }
            if target
                .has_overlap(clip.timeline, None)
                .map_err(|error| Rejection::new(error.to_string()))?
            {
                return Err(Rejection::new(format!(
                    "audio track {} already has a clip overlapping clip {}'s exact timeline \
                     range; choose or add a free audio track",
                    args.track, args.clip
                )));
            }
            EditCommand::ExtractAudio {
                clip: clip.id,
                to_track: Some(target.id),
            }
        }
        WireCommand::DuplicateClip(args) => {
            let clip = clip_ref(project, args.clip)?;
            let target = track_ref(project, args.to_track)?;
            if !target.kind.accepts_content(&clip.content) {
                return Err(Rejection::new(format!(
                    "clip {} cannot be duplicated onto track {} ({} lane); choose a track \
                     compatible with the source clip's content",
                    args.clip,
                    args.to_track,
                    kind_name(target.kind),
                )));
            }
            if target.locked {
                return Err(Rejection::new(format!(
                    "destination track {} is locked; unlock it or choose another compatible track",
                    args.to_track
                )));
            }
            let start = timeline_time(project, args.start, "start")?;
            require_non_negative(args.start, "start")?;
            let mut destination = clip.timeline;
            destination.start = start;
            if target
                .has_overlap(destination, None)
                .map_err(|error| Rejection::new(error.to_string()))?
            {
                let rate = timeline_rate(project);
                return Err(Rejection::new(format!(
                    "track {} has a clip overlapping duplicate of clip {} in the exact \
                     destination range {:.3}s to {:.3}s; choose another target track or a start \
                     with {:.3}s of free space — duplicate_clip does not ripple or search for space",
                    args.to_track,
                    args.clip,
                    ticks_to_seconds(destination.start.value, rate),
                    ticks_to_seconds(destination.end_tick(), rate),
                    ticks_to_seconds(destination.duration.value, rate),
                )));
            }
            EditCommand::DuplicateClip {
                clip: clip.id,
                to_track: target.id,
                start,
            }
        }
        WireCommand::AddGenerated(args) => {
            let track = track_ref(project, args.track)?;
            let generator = lower_generator(&args.generator, None);
            require_generator_lane(track, &generator)?;
            let timeline = timeline_range(project, args.start, args.duration)?;
            EditCommand::AddGenerated {
                track: track.id,
                generator,
                timeline,
            }
        }
        WireCommand::SetGenerator(args) => {
            let clip = clip_ref(project, args.clip)?;
            let Some(current) = generated_content(clip) else {
                return Err(Rejection::new(format!(
                    "clip {} is a media clip; set_generator only works on generated \
                     clips (text, solid, shape)",
                    args.clip
                )));
            };
            EditCommand::SetGenerator {
                clip: clip.id,
                generator: lower_generator(&args.generator, Some(current)),
            }
        }
        WireCommand::SetClipTransform(args) => {
            let clip = clip_ref(project, args.clip)?;
            // Omitted properties keep their current value — sampled at the
            // clip start for animated params (the agent edits whole-clip
            // placement; keyframe-level edits get their own commands).
            let current = clip.transform.sample(0);
            let transform = ClipTransform {
                position: [
                    args.position_x.map_or(current.position[0], |v| v as f32),
                    args.position_y.map_or(current.position[1], |v| v as f32),
                ],
                anchor_point: [
                    args.anchor_x.map_or(current.anchor_point[0], |v| v as f32),
                    args.anchor_y.map_or(current.anchor_point[1], |v| v as f32),
                ],
                scale: args.scale.map_or(current.scale, |v| v as f32),
                rotation: args.rotation.map_or(current.rotation, |v| v as f32),
                opacity: args.opacity.map_or(current.opacity, |v| v as f32),
            };
            transform
                .validate()
                .map_err(|e| Rejection::new(format!("invalid transform: {e}")))?;
            EditCommand::SetClipTransform {
                clip: clip.id,
                transform,
                at: None,
            }
        }
        WireCommand::SetClipCrop(args) => {
            let clip = clip_ref(project, args.clip)?;
            let timeline = project.timeline();
            let on_audio_lane = timeline
                .track_of(clip.id)
                .and_then(|id| timeline.track(id))
                .is_some_and(|t| t.kind == TrackKind::Audio);
            if on_audio_lane {
                return Err(Rejection::new(format!(
                    "clip {} is on an audio lane; there is no frame to crop",
                    args.clip
                )));
            }
            // Omitted edges keep the clip's current framing. Current insets
            // derive from the stored kept-region rect.
            let current = clip.crop;
            let inset = |requested: Option<f64>, current: f32, what: &str| {
                let Some(v) = requested else {
                    return Ok(current);
                };
                if !v.is_finite() || !(0.0..1.0).contains(&v) {
                    return Err(Rejection::new(format!(
                        "{what} must be a fraction between 0 and 1 (got {v})"
                    )));
                }
                Ok(v as f32)
            };
            let left = inset(args.left, current.x, "left")?;
            let top = inset(args.top, current.y, "top")?;
            let right = inset(args.right, 1.0 - current.x - current.w, "right")?;
            let bottom = inset(args.bottom, 1.0 - current.y - current.h, "bottom")?;
            let crop = CropRect {
                x: left,
                y: top,
                w: 1.0 - left - right,
                h: 1.0 - top - bottom,
            };
            crop.validate().map_err(|_| {
                Rejection::new(format!(
                    "crop leaves no visible frame: left {left} + right {right} and \
                     top {top} + bottom {bottom} must each keep at least 1% of the frame"
                ))
            })?;
            EditCommand::SetClipCrop {
                clip: clip.id,
                crop,
                flip_h: args.flip_h.unwrap_or(clip.flip_h),
                flip_v: args.flip_v.unwrap_or(clip.flip_v),
            }
        }
        WireCommand::AddEffect(args) => {
            let clip = clip_ref(project, args.clip)?;
            reject_audio_lane(project, clip, "effects need a visual frame", args.clip)?;
            if cutlass_models::effect_spec(&args.effect).is_none() {
                return Err(Rejection::new(format!(
                    "unknown effect '{}'; available effects: {}",
                    args.effect,
                    effect_ids()
                )));
            }
            EditCommand::AddEffect {
                clip: clip.id,
                effect_id: args.effect.clone(),
            }
        }
        WireCommand::RemoveEffect(args) => {
            let clip = clip_ref(project, args.clip)?;
            let index = effect_index(clip, args.index, args.clip)?;
            EditCommand::RemoveEffect {
                clip: clip.id,
                index,
            }
        }
        WireCommand::MoveEffect(args) => {
            let clip = clip_ref(project, args.clip)?;
            let from_index = effect_index(clip, args.from_index, args.clip)?;
            let to_index = effect_index(clip, args.to_index, args.clip)?;
            if from_index == to_index {
                return Err(Rejection::new(format!(
                    "effect {from_index} on clip {} is already at index {to_index}; \
                     move_effect would make no change",
                    args.clip
                )));
            }
            EditCommand::MoveEffect {
                clip: clip.id,
                from_index,
                to_index,
            }
        }
        WireCommand::SetEffectParam(args) => {
            let clip = clip_ref(project, args.clip)?;
            let index = effect_index(clip, args.index, args.clip)?;
            let instance = &clip.effects[index];
            let spec = cutlass_models::effect_spec(&instance.effect_id).ok_or_else(|| {
                Rejection::new(format!(
                    "effect '{}' on clip {} is not in the catalog",
                    instance.effect_id, args.clip
                ))
            })?;
            let slot = spec
                .params
                .iter()
                .position(|p| p.name == args.param)
                .ok_or_else(|| {
                    let names: Vec<&str> = spec.params.iter().map(|p| p.name).collect();
                    Rejection::new(format!(
                        "effect '{}' has no parameter '{}'; parameters: {}",
                        instance.effect_id,
                        args.param,
                        names.join(", ")
                    ))
                })?;
            let p = &spec.params[slot];
            let v = args.value;
            if !v.is_finite() || v < f64::from(p.min) || v > f64::from(p.max) {
                return Err(Rejection::new(format!(
                    "{} must be between {} and {} (got {})",
                    args.param, p.min, p.max, v
                )));
            }
            EditCommand::SetEffectParam {
                clip: clip.id,
                index,
                param: slot,
                value: v as f32,
            }
        }
        WireCommand::AddTransition(args) => {
            let clip = clip_ref(project, args.clip)?;
            reject_non_transition_lane(project, clip, args.clip)?;
            if cutlass_models::transition_spec(&args.transition).is_none() {
                return Err(Rejection::new(format!(
                    "unknown transition '{}'; available transitions: {}",
                    args.transition,
                    transition_ids()
                )));
            }
            if !has_right_neighbor(project, clip) {
                return Err(Rejection::new(format!(
                    "clip {} has no clip butting against its right edge to transition into",
                    args.clip
                )));
            }
            EditCommand::AddTransition {
                clip: clip.id,
                transition_id: args.transition.clone(),
            }
        }
        WireCommand::RemoveTransition(args) => {
            let clip = clip_ref(project, args.clip)?;
            require_transition(project, clip, args.clip)?;
            EditCommand::RemoveTransition { clip: clip.id }
        }
        WireCommand::SetTransition(args) => {
            let clip = clip_ref(project, args.clip)?;
            require_transition(project, clip, args.clip)?;
            if !(args.seconds.is_finite()) || args.seconds <= 0.0 {
                return Err(Rejection::new(format!(
                    "transition duration must be positive (got {}s)",
                    args.seconds
                )));
            }
            let duration =
                seconds_to_ticks(args.seconds, timeline_rate(project), "duration")?.max(1);
            EditCommand::SetTransition {
                clip: clip.id,
                duration,
            }
        }
        WireCommand::SetParamKeyframe(args) => {
            let clip = clip_ref(project, args.clip)?;
            let at = keyframe_position(project, clip, args.at)?;
            let value = param_value(args.param, args.value, args.position)?;
            EditCommand::SetParamKeyframe {
                clip: clip.id,
                param: clip_param(args.param),
                at,
                value,
                easing: easing(args.easing),
            }
        }
        WireCommand::RemoveParamKeyframe(args) => {
            let clip = clip_ref(project, args.clip)?;
            let at = keyframe_position(project, clip, args.at)?;
            EditCommand::RemoveParamKeyframe {
                clip: clip.id,
                param: clip_param(args.param),
                at,
            }
        }
        WireCommand::SetClipSpeed(args) => {
            let clip = clip_ref(project, args.clip)?;
            if clip.is_generated() {
                return Err(Rejection::new(format!(
                    "clip {} is a generated clip; set_clip_speed only works on media \
                     clips (footage with a source file)",
                    args.clip
                )));
            }
            // Omitted fields keep the clip's current retiming.
            let speed = match args.speed {
                Some(speed) => rational_speed(speed)?,
                None => clip.speed,
            };
            EditCommand::SetClipSpeed {
                clip: clip.id,
                speed,
                reversed: args.reversed.unwrap_or(clip.reversed),
            }
        }
        WireCommand::SetSpeedCurve(args) => {
            let clip = clip_ref(project, args.clip)?;
            if clip.is_generated() {
                return Err(Rejection::new(format!(
                    "clip {} is a generated clip; set_speed_curve only works on media \
                     clips (footage with a source file)",
                    args.clip
                )));
            }
            let curve = match &args.preset {
                Some(name) => Some(cutlass_models::speed_preset(name).ok_or_else(|| {
                    Rejection::new(format!(
                        "unknown speed ramp preset '{name}'; choose one of: \
                         ramp_up, ramp_down, montage, hero, bullet"
                    ))
                })?),
                None => None,
            };
            EditCommand::SetSpeedCurve {
                clip: clip.id,
                curve,
            }
        }
        WireCommand::SetClipPitch(args) => {
            let clip = clip_ref(project, args.clip)?;
            if clip.is_generated() {
                return Err(Rejection::new(format!(
                    "clip {} is a generated clip; set_clip_pitch only works on media \
                     clips (footage with a source file)",
                    args.clip
                )));
            }
            EditCommand::SetClipPitch {
                clip: clip.id,
                preserve_pitch: args.preserve_pitch,
            }
        }
        WireCommand::SetDenoise(args) => {
            let clip = clip_ref(project, args.clip)?;
            if clip.is_generated() {
                return Err(Rejection::new(format!(
                    "clip {} is a generated clip; set_denoise only works on media \
                     clips (footage with a source file)",
                    args.clip
                )));
            }
            // CapCut keeps a video's sound on the clip itself, so denoise lands
            // on the clip directly — unless its audio was detached to a linked
            // audio lane, where the audible half now lives (same rule as
            // set_clip_audio).
            let timeline = project.timeline();
            if !timeline.carries_own_audio(clip.id) {
                let companion = clip.link.and_then(|link| {
                    timeline
                        .tracks_ordered()
                        .filter(|t| t.kind == TrackKind::Audio)
                        .flat_map(|t| t.clips())
                        .find(|c| c.link == Some(link))
                        .map(|c| c.id.raw())
                });
                return Err(Rejection::new(match companion {
                    Some(id) => format!(
                        "clip {} is not on an audio lane; its audio plays through \
                         linked clip {id} — call set_denoise on clip {id} instead",
                        args.clip
                    ),
                    None => format!(
                        "clip {} is not on an audio lane and has no linked audio \
                         companion; there is nothing audible to clean",
                        args.clip
                    ),
                }));
            }
            EditCommand::SetClipDenoise {
                clip: clip.id,
                denoise: args.denoise,
            }
        }
        WireCommand::SetClipMask(args) => {
            let clip = clip_ref(project, args.clip)?;
            reject_audio_lane(project, clip, "masks need a visual frame", args.clip)?;
            if clip.is_generated() {
                return Err(Rejection::new(format!(
                    "clip {} is a generated clip; set_clip_mask only works on media \
                     clips (footage with a source file)",
                    args.clip
                )));
            }
            let mask = match &args.mask {
                None => None,
                Some(wire) => Some(lower_mask(wire)?),
            };
            EditCommand::SetClipMask {
                clip: clip.id,
                mask,
            }
        }
        WireCommand::SetClipChroma(args) => {
            let clip = clip_ref(project, args.clip)?;
            reject_audio_lane(project, clip, "chroma key needs a visual frame", args.clip)?;
            if clip.is_generated() {
                return Err(Rejection::new(format!(
                    "clip {} is a generated clip; set_clip_chroma only works on media \
                     clips (footage with a source file)",
                    args.clip
                )));
            }
            let chroma = match &args.chroma {
                None => None,
                Some(wire) => Some(lower_chroma(wire)?),
            };
            EditCommand::SetClipChroma {
                clip: clip.id,
                chroma,
            }
        }
        WireCommand::SetClipStabilize(args) => {
            let clip = clip_ref(project, args.clip)?;
            reject_audio_lane(
                project,
                clip,
                "stabilization needs a visual frame",
                args.clip,
            )?;
            if clip.is_generated() {
                return Err(Rejection::new(format!(
                    "clip {} is a generated clip; set_clip_stabilize only works on media \
                     clips (footage with a source file)",
                    args.clip
                )));
            }
            if let cutlass_models::ClipSource::Media { media, .. } = &clip.content
                && project.media(*media).is_some_and(|m| m.is_image)
            {
                return Err(Rejection::new(format!(
                    "clip {} is a still image; stabilization requires video",
                    args.clip
                )));
            }
            let stabilize = args.level.map(lower_stabilize);
            EditCommand::SetClipStabilize {
                clip: clip.id,
                stabilize,
            }
        }
        WireCommand::SetClipFilter(args) => {
            let clip = clip_ref(project, args.clip)?;
            reject_audio_lane(project, clip, "filters need a visual frame", args.clip)?;
            let filter = match &args.filter {
                None => None,
                Some(wire) => Some(lower_filter(wire)?),
            };
            EditCommand::SetClipFilter {
                clip: clip.id,
                filter,
            }
        }
        WireCommand::SetClipAdjustments(args) => {
            let clip = clip_ref(project, args.clip)?;
            reject_audio_lane(project, clip, "adjustments need a visual frame", args.clip)?;
            let mut adjust = clip.adjust;
            if let Some(v) = args.brightness {
                adjust.brightness = unit_slider(v, "brightness")?;
            }
            if let Some(v) = args.contrast {
                adjust.contrast = unit_slider(v, "contrast")?;
            }
            if let Some(v) = args.saturation {
                adjust.saturation = unit_slider(v, "saturation")?;
            }
            if let Some(v) = args.exposure {
                adjust.exposure = unit_slider(v, "exposure")?;
            }
            if let Some(v) = args.temperature {
                adjust.temperature = unit_slider(v, "temperature")?;
            }
            adjust
                .validate()
                .map_err(|e| Rejection::new(e.to_string()))?;
            EditCommand::SetClipAdjustments {
                clip: clip.id,
                adjust,
            }
        }
        WireCommand::SetClipAnimation(args) => {
            let clip = clip_ref(project, args.clip)?;
            reject_audio_lane(project, clip, "animations need a visual frame", args.clip)?;
            let slot = lower_animation_slot(args.slot);
            let animation = match &args.animation {
                None => None,
                Some(id) => {
                    let spec = animation_spec(id).ok_or_else(|| {
                        Rejection::new(format!(
                            "unknown animation '{id}'; available animations include fade_in, \
                             fade_out, pulse, slide_up, zoom_in, and others from the catalog"
                        ))
                    })?;
                    if spec.slot != slot {
                        return Err(Rejection::new(format!(
                            "animation '{id}' does not fit the {} slot",
                            match args.slot {
                                WireAnimationSlot::In => "in",
                                WireAnimationSlot::Out => "out",
                                WireAnimationSlot::Combo => "combo",
                            }
                        )));
                    }
                    if spec.text_only
                        && !matches!(
                            clip.content,
                            cutlass_models::ClipSource::Generated(Generator::Text { .. })
                        )
                    {
                        return Err(Rejection::new(format!(
                            "animation '{id}' is a text-only preset"
                        )));
                    }
                    Some(AnimationRef::new(id.clone()))
                }
            };
            EditCommand::SetClipAnimation {
                clip: clip.id,
                slot,
                animation,
            }
        }
        WireCommand::SetAudioRole(args) => {
            let clip = clip_ref(project, args.clip)?;
            let timeline = project.timeline();
            let on_audio = timeline
                .track_of(clip.id)
                .and_then(|id| timeline.track(id))
                .is_some_and(|t| t.kind == TrackKind::Audio);
            if !on_audio {
                return Err(Rejection::new(format!(
                    "clip {} is not on an audio lane; set_audio_role only works on audio clips",
                    args.clip
                )));
            }
            EditCommand::SetAudioRole {
                clip: clip.id,
                role: args.role.map(lower_audio_role),
            }
        }
        WireCommand::SetClipAudio(args) => {
            let clip = clip_ref(project, args.clip)?;
            if clip.is_generated() {
                return Err(Rejection::new(format!(
                    "clip {} is a generated clip; set_clip_audio only works on media \
                     clips (footage with a source file)",
                    args.clip
                )));
            }
            // CapCut keeps a video's sound on the clip itself, so volume/fades
            // land on the clip directly — unless its audio was detached to a
            // linked audio lane, where the audible half now lives.
            let timeline = project.timeline();
            if !timeline.carries_own_audio(clip.id) {
                let companion = clip.link.and_then(|link| {
                    timeline
                        .tracks_ordered()
                        .filter(|t| t.kind == TrackKind::Audio)
                        .flat_map(|t| t.clips())
                        .find(|c| c.link == Some(link))
                        .map(|c| c.id.raw())
                });
                return Err(Rejection::new(match companion {
                    Some(id) => format!(
                        "clip {} is not on an audio lane; its audio plays through \
                         linked clip {id} — call set_clip_audio on clip {id} instead",
                        args.clip
                    ),
                    None => format!(
                        "clip {} is not on an audio lane and has no linked audio \
                         companion; there is nothing audible to adjust",
                        args.clip
                    ),
                }));
            }
            // Omitted volume keeps the clip's gain untouched — a flat level
            // stays flat and, crucially, an envelope is preserved (so "fade
            // the music out" past a keyframed clip doesn't wipe the
            // automation). A present volume sets a flat level (basic slider).
            let volume = match args.volume {
                Some(volume) => {
                    if !volume.is_finite() || !(0.0..=10.0).contains(&volume) {
                        return Err(Rejection::new(format!(
                            "volume must be between 0 (mute) and 10 (got {volume})"
                        )));
                    }
                    Some(volume as f32)
                }
                None => None,
            };
            let rate = timeline_rate(project);
            let clip_ticks = clip.timeline.duration.value;
            let fade = |current: i64,
                        requested: Option<f64>,
                        what: &str|
             -> Result<RationalTime, Rejection> {
                let Some(seconds) = requested else {
                    return Ok(RationalTime::new(current, rate));
                };
                require_non_negative(seconds, what)?;
                let time = timeline_time(project, seconds, what)?;
                if time.value > clip_ticks {
                    return Err(Rejection::new(format!(
                        "{what} of {seconds}s is longer than clip {} ({:.3}s)",
                        args.clip,
                        ticks_to_seconds(clip_ticks, rate),
                    )));
                }
                Ok(time)
            };
            EditCommand::SetClipAudio {
                clip: clip.id,
                volume,
                fade_in: fade(clip.fade_in, args.fade_in, "fade_in")?,
                fade_out: fade(clip.fade_out, args.fade_out, "fade_out")?,
            }
        }
        WireCommand::SetParamConstant(args) => {
            let clip = clip_ref(project, args.clip)?;
            let value = param_value(args.param, args.value, args.position)?;
            EditCommand::SetParamConstant {
                clip: clip.id,
                param: clip_param(args.param),
                value,
            }
        }
        WireCommand::SplitClip(args) => {
            let clip = clip_ref(project, args.clip)?;
            let at = timeline_time(project, args.at, "at")?;
            let tl = clip.timeline;
            if at.value <= tl.start.value || at.value >= tl.end_tick() {
                let rate = timeline_rate(project);
                return Err(Rejection::new(format!(
                    "split position {:.3}s is not strictly inside clip {} \
                     ({:.3}s to {:.3}s)",
                    args.at,
                    args.clip,
                    ticks_to_seconds(tl.start.value, rate),
                    ticks_to_seconds(tl.end_tick(), rate),
                )));
            }
            EditCommand::SplitClip { clip: clip.id, at }
        }
        WireCommand::TrimClip(args) => {
            let clip = clip_ref(project, args.clip)?;
            let timeline = timeline_range(project, args.start, args.duration)?;
            EditCommand::TrimClip {
                clip: clip.id,
                timeline,
            }
        }
        WireCommand::MoveClip(args) => {
            let clip = clip_ref(project, args.clip)?;
            let track = track_ref(project, args.to_track)?;
            if !track.kind.accepts_content(&clip.content) {
                return Err(Rejection::new(format!(
                    "clip {} cannot live on track {} ({} lane)",
                    args.clip,
                    args.to_track,
                    kind_name(track.kind),
                )));
            }
            let start = timeline_time(project, args.start, "start")?;
            require_non_negative(args.start, "start")?;
            EditCommand::MoveClip {
                clip: clip.id,
                to_track: track.id,
                start,
            }
        }
        WireCommand::RemoveClip(args) => EditCommand::RemoveClip {
            clip: clip_ref(project, args.clip)?.id,
        },
        WireCommand::RemoveTrack(args) => {
            let track = track_ref(project, args.track)?;
            if track.main {
                return Err(Rejection::new(format!(
                    "track {} is the main track and cannot be removed; \
                     it is the permanent video lane every edit builds on",
                    args.track,
                )));
            }
            EditCommand::RemoveTrack { track: track.id }
        }
        WireCommand::SetTrackEnabled(args) => EditCommand::SetTrackEnabled {
            track: track_ref(project, args.track)?.id,
            enabled: args.enabled,
        },
        WireCommand::SetTrackMuted(args) => EditCommand::SetTrackMuted {
            track: track_ref(project, args.track)?.id,
            muted: args.muted,
        },
        WireCommand::SetTrackLocked(args) => EditCommand::SetTrackLocked {
            track: track_ref(project, args.track)?.id,
            locked: args.locked,
        },
        WireCommand::RippleDelete(args) => EditCommand::RippleDelete {
            clip: clip_ref(project, args.clip)?.id,
        },
        WireCommand::ShiftClips(args) => {
            let track = track_ref(project, args.track)?;
            require_non_negative(args.from, "from")?;
            let from = timeline_time(project, args.from, "from")?;
            let delta = timeline_time_signed(project, args.delta, "delta")?;
            if delta.value == 0 {
                return Err(Rejection::new(format!(
                    "delta of {:+.4}s rounds to zero frames at the timeline rate; \
                     nothing would move",
                    args.delta
                )));
            }
            EditCommand::ShiftClips {
                track: track.id,
                from,
                delta,
            }
        }
        WireCommand::RippleInsert(args) => {
            let track = track_ref(project, args.track)?;
            let media = media_ref(project, args.media)?;
            require_media_lane(track.kind, args.track)?;
            let (source, at) = media_placement(
                project,
                media,
                args.source_start,
                args.source_duration,
                args.at,
                "at",
            )?;
            EditCommand::RippleInsert {
                track: track.id,
                media: media.id,
                source,
                at,
            }
        }
        WireCommand::LinkClips(args) => {
            if args.clips.len() < 2 {
                return Err(Rejection::new(
                    "link_clips needs at least two clip ids".to_string(),
                ));
            }
            if args.clips.len() > MAX_MULTI_CLIP_REFS {
                return Err(Rejection::new(format!(
                    "link_clips accepts at most {MAX_MULTI_CLIP_REFS} clip ids (got {})",
                    args.clips.len()
                )));
            }
            let mut clips = Vec::with_capacity(args.clips.len());
            for &raw in &args.clips {
                clips.push(clip_ref(project, raw)?.id);
            }
            EditCommand::LinkClips { clips }
        }
        WireCommand::UnlinkClips(args) => {
            if args.clips.is_empty() {
                return Err(Rejection::new(
                    "unlink_clips needs at least one clip id".to_string(),
                ));
            }
            if args.clips.len() > MAX_MULTI_CLIP_REFS {
                return Err(Rejection::new(format!(
                    "unlink_clips accepts at most {MAX_MULTI_CLIP_REFS} clip ids (got {})",
                    args.clips.len()
                )));
            }
            let mut seen = HashSet::with_capacity(args.clips.len());
            for &raw in &args.clips {
                if !seen.insert(raw) {
                    return Err(Rejection::new(format!(
                        "unlink_clips contains duplicate clip id {raw}; list each clip once"
                    )));
                }
            }

            let mut clips = Vec::with_capacity(args.clips.len());
            let mut any_linked = false;
            for &raw in &args.clips {
                let clip = clip_ref(project, raw)?;
                any_linked |= clip.link.is_some();
                clips.push(clip.id);
            }
            if !any_linked {
                return Err(Rejection::new(
                    "unlink_clips found no linked clips; all referenced clips are already unlinked",
                ));
            }
            EditCommand::UnlinkClips { clips }
        }
        WireCommand::AddMarker(args) => {
            require_non_negative(args.at, "at")?;
            let at = timeline_time(project, args.at, "at")?;
            EditCommand::AddMarker {
                at,
                name: args.name.clone().unwrap_or_default(),
                color: args.color.map(marker_color),
            }
        }
        WireCommand::RemoveMarker(args) => EditCommand::RemoveMarker {
            marker: marker_ref(project, args.marker)?.id,
        },
        WireCommand::SetMarker(args) => {
            let marker = marker_ref(project, args.marker)?;
            let at = match args.at {
                Some(seconds) => {
                    require_non_negative(seconds, "at")?;
                    timeline_time(project, seconds, "at")?
                }
                None => marker.tick,
            };
            EditCommand::SetMarker {
                marker: marker.id,
                at,
                name: args.name.clone().unwrap_or_else(|| marker.name.clone()),
                color: args.color.map(marker_color).unwrap_or(marker.color),
            }
        }
        WireCommand::SetCanvas(args) => {
            let current = project.timeline().canvas();
            EditCommand::SetCanvas {
                aspect: args.aspect.map(canvas_aspect).unwrap_or(current.aspect),
                background: args.background.unwrap_or(current.background),
            }
        }
    };
    Ok(Command::Edit(edit))
}

// --- entity lookups (errors list what exists) ------------------------------

const MAX_LISTED_IDS: usize = 32;

fn list_ids(mut ids: Vec<u64>) -> String {
    if ids.is_empty() {
        return "none".to_string();
    }
    ids.sort_unstable();
    let extra = ids.len().saturating_sub(MAX_LISTED_IDS);
    let mut out = ids
        .into_iter()
        .take(MAX_LISTED_IDS)
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if extra > 0 {
        out.push_str(&format!(" (and {extra} more)"));
    }
    out
}

fn unit_slider(value: f64, name: &str) -> Result<f32, Rejection> {
    if !value.is_finite() || !(-1.0..=1.0).contains(&value) {
        return Err(Rejection::new(format!(
            "{name} must be between -1 and 1 (got {value})"
        )));
    }
    Ok(value as f32)
}

fn lower_mask_kind(kind: WireMaskKind) -> MaskKind {
    match kind {
        WireMaskKind::Linear => MaskKind::Linear,
        WireMaskKind::Mirror => MaskKind::Mirror,
        WireMaskKind::Circle => MaskKind::Circle,
        WireMaskKind::Rectangle => MaskKind::Rectangle,
        WireMaskKind::Heart => MaskKind::Heart,
        WireMaskKind::Star => MaskKind::Star,
    }
}

fn lower_mask(wire: &WireMask) -> Result<Mask, Rejection> {
    let feather = wire.feather.unwrap_or(0.0);
    if !feather.is_finite() || !(0.0..=1.0).contains(&feather) {
        return Err(Rejection::new(format!(
            "mask feather must be between 0 and 1 (got {feather})"
        )));
    }
    let mask = Mask {
        kind: lower_mask_kind(wire.kind),
        feather: feather as f32,
        invert: wire.invert.unwrap_or(false),
    };
    mask.validate().map_err(|e| Rejection::new(e.to_string()))?;
    Ok(mask)
}

fn lower_chroma(wire: &WireChromaKey) -> Result<ChromaKey, Rejection> {
    let strength = wire.strength.unwrap_or(0.0);
    let shadow = wire.shadow.unwrap_or(0.0);
    for (name, value) in [("strength", strength), ("shadow", shadow)] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(Rejection::new(format!(
                "chroma {name} must be between 0 and 1 (got {value})"
            )));
        }
    }
    let chroma = ChromaKey {
        rgb: wire.rgb,
        strength: strength as f32,
        shadow: shadow as f32,
    };
    chroma
        .validate()
        .map_err(|e| Rejection::new(e.to_string()))?;
    Ok(chroma)
}

fn lower_stabilize(level: WireStabilizeLevel) -> StabilizeLevel {
    match level {
        WireStabilizeLevel::Recommended => StabilizeLevel::Recommended,
        WireStabilizeLevel::Smooth => StabilizeLevel::Smooth,
        WireStabilizeLevel::MaxSmooth => StabilizeLevel::MaxSmooth,
    }
}

fn lower_filter(wire: &crate::wire::WireFilter) -> Result<Filter, Rejection> {
    let intensity = wire.intensity.unwrap_or(0.8);
    if !intensity.is_finite() || !(0.0..=1.0).contains(&intensity) {
        return Err(Rejection::new(format!(
            "filter intensity must be between 0 and 1 (got {intensity})"
        )));
    }
    let filter = Filter {
        id: wire.id.clone(),
        intensity: intensity as f32,
    };
    filter.validate().map_err(|e| {
        let ids = filter_catalog()
            .iter()
            .map(|s| s.id)
            .collect::<Vec<_>>()
            .join(", ");
        if filter_spec(&wire.id).is_none() {
            Rejection::new(format!(
                "unknown filter '{}'; available filters: {ids}",
                wire.id
            ))
        } else {
            Rejection::new(e.to_string())
        }
    })?;
    Ok(filter)
}

fn lower_animation_slot(slot: WireAnimationSlot) -> AnimationSlot {
    match slot {
        WireAnimationSlot::In => AnimationSlot::In,
        WireAnimationSlot::Out => AnimationSlot::Out,
        WireAnimationSlot::Combo => AnimationSlot::Combo,
    }
}

fn lower_audio_role(role: WireAudioRole) -> AudioRole {
    match role {
        WireAudioRole::Music => AudioRole::Music,
        WireAudioRole::Sfx => AudioRole::Sfx,
        WireAudioRole::Voiceover => AudioRole::Voiceover,
        WireAudioRole::Extracted => AudioRole::Extracted,
    }
}

fn clip_ref(project: &Project, raw: u64) -> Result<&Clip, Rejection> {
    project.clip(ClipId::from_raw(raw)).ok_or_else(|| {
        let existing = project
            .timeline()
            .tracks_ordered()
            .flat_map(|t| t.clips())
            .map(|c| c.id.raw())
            .collect();
        Rejection::new(format!(
            "clip {raw} does not exist; clips on the timeline: {}",
            list_ids(existing)
        ))
    })
}

/// Comma-separated catalog effect ids, for rejection messages.
fn effect_ids() -> String {
    cutlass_models::effect_catalog()
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>()
        .join(", ")
}

fn transition_ids() -> String {
    cutlass_models::transition_catalog()
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether a clip butts directly against a following clip on its track (the
/// next clip starts exactly where this one ends) — the precondition for a
/// transition at its right junction.
fn has_right_neighbor(project: &Project, clip: &Clip) -> bool {
    let timeline = project.timeline();
    let Some(track) = timeline.track_of(clip.id).and_then(|id| timeline.track(id)) else {
        return false;
    };
    let end = clip.timeline.end_tick();
    track
        .clips()
        .any(|c| c.id != clip.id && c.timeline.start.value == end)
}

/// Reject when the clip carries no transition at its right junction.
fn require_transition(project: &Project, clip: &Clip, raw_clip: u64) -> Result<(), Rejection> {
    let timeline = project.timeline();
    let has = timeline
        .track_of(clip.id)
        .and_then(|id| timeline.track(id))
        .is_some_and(|t| t.transition_at(clip.id).is_some());
    if !has {
        return Err(Rejection::new(format!(
            "clip {raw_clip} has no transition at its right cut"
        )));
    }
    Ok(())
}

/// Resolve a wire effect-chain index against a clip, rejecting out-of-range
/// indices with the clip's current chain length.
fn effect_index(clip: &Clip, index: u32, raw_clip: u64) -> Result<usize, Rejection> {
    let index = usize::try_from(index).map_err(|_| {
        Rejection::new(format!(
            "clip {raw_clip} has no effect at index {index} (chain length {})",
            clip.effects.len()
        ))
    })?;
    if index >= clip.effects.len() {
        return Err(Rejection::new(format!(
            "clip {raw_clip} has no effect at index {index} (chain length {})",
            clip.effects.len()
        )));
    }
    Ok(index)
}

/// Reject clips on an audio lane, which have no frame to operate on.
fn reject_audio_lane(
    project: &Project,
    clip: &Clip,
    why: &str,
    raw_clip: u64,
) -> Result<(), Rejection> {
    let timeline = project.timeline();
    let on_audio = timeline
        .track_of(clip.id)
        .and_then(|id| timeline.track(id))
        .is_some_and(|t| t.kind == TrackKind::Audio);
    if on_audio {
        return Err(Rejection::new(format!(
            "clip {raw_clip} is on an audio lane; {why}"
        )));
    }
    Ok(())
}

/// Reject transitions on lanes whose clips don't composite as frames: audio
/// has no frame, and effect/filter/adjustment segments resolve to canvas-wide
/// passes the renderer can't nest inside a transition (mirrors the model's
/// `TrackKind::supports_transitions`).
fn reject_non_transition_lane(
    project: &Project,
    clip: &Clip,
    raw_clip: u64,
) -> Result<(), Rejection> {
    let timeline = project.timeline();
    let kind = timeline
        .track_of(clip.id)
        .and_then(|id| timeline.track(id))
        .map(|t| t.kind);
    match kind {
        Some(kind) if !kind.supports_transitions() => Err(Rejection::new(format!(
            "clip {raw_clip} is on a {kind:?} lane; transitions need a lane that composites frames (video, sticker, or text)"
        ))),
        _ => Ok(()),
    }
}

fn track_ref(project: &Project, raw: u64) -> Result<&cutlass_models::Track, Rejection> {
    project
        .timeline()
        .track(TrackId::from_raw(raw))
        .ok_or_else(|| {
            let existing = project
                .timeline()
                .tracks_ordered()
                .map(|t| t.id.raw())
                .collect();
            Rejection::new(format!(
                "track {raw} does not exist; tracks on the timeline: {}",
                list_ids(existing)
            ))
        })
}

fn media_ref(project: &Project, raw: u64) -> Result<&cutlass_models::MediaSource, Rejection> {
    project.media(MediaId::from_raw(raw)).ok_or_else(|| {
        let existing = project.media_iter().map(|m| m.id.raw()).collect();
        Rejection::new(format!(
            "media {raw} does not exist; media in the pool: {}",
            list_ids(existing)
        ))
    })
}

fn marker_ref(project: &Project, raw: u64) -> Result<&Marker, Rejection> {
    project
        .timeline()
        .marker(MarkerId::from_raw(raw))
        .ok_or_else(|| {
            let existing = project
                .timeline()
                .markers()
                .iter()
                .map(|m| m.id.raw())
                .collect();
            Rejection::new(format!(
                "marker {raw} does not exist; markers on the timeline: {}",
                list_ids(existing)
            ))
        })
}

fn marker_color(color: WireMarkerColor) -> MarkerColor {
    match color {
        WireMarkerColor::Teal => MarkerColor::Teal,
        WireMarkerColor::Blue => MarkerColor::Blue,
        WireMarkerColor::Purple => MarkerColor::Purple,
        WireMarkerColor::Pink => MarkerColor::Pink,
        WireMarkerColor::Red => MarkerColor::Red,
        WireMarkerColor::Orange => MarkerColor::Orange,
        WireMarkerColor::Yellow => MarkerColor::Yellow,
        WireMarkerColor::Green => MarkerColor::Green,
    }
}

fn canvas_aspect(aspect: WireCanvasAspect) -> CanvasAspect {
    match aspect {
        WireCanvasAspect::Auto => CanvasAspect::Auto,
        WireCanvasAspect::Wide16x9 => CanvasAspect::Wide16x9,
        WireCanvasAspect::Tall9x16 => CanvasAspect::Tall9x16,
        WireCanvasAspect::Square1x1 => CanvasAspect::Square1x1,
        WireCanvasAspect::Portrait4x5 => CanvasAspect::Portrait4x5,
        WireCanvasAspect::Cinema21x9 => CanvasAspect::Cinema21x9,
    }
}

// --- kind rules -------------------------------------------------------------

fn track_kind(kind: WireTrackKind) -> TrackKind {
    match kind {
        WireTrackKind::Video => TrackKind::Video,
        WireTrackKind::Audio => TrackKind::Audio,
        WireTrackKind::Text => TrackKind::Text,
        WireTrackKind::Sticker => TrackKind::Sticker,
    }
}

fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Text => "text",
        TrackKind::Sticker => "sticker",
        TrackKind::Effect => "effect",
        TrackKind::Filter => "filter",
        TrackKind::Adjustment => "adjustment",
    }
}

fn require_media_lane(kind: TrackKind, raw: u64) -> Result<(), Rejection> {
    if matches!(kind, TrackKind::Video | TrackKind::Audio) {
        Ok(())
    } else {
        Err(Rejection::new(format!(
            "track {raw} is a {} lane; media clips need a video or audio track",
            kind_name(kind)
        )))
    }
}

fn require_generator_lane(
    track: &cutlass_models::Track,
    generator: &Generator,
) -> Result<(), Rejection> {
    let content = cutlass_models::ClipSource::Generated(generator.clone());
    if track.kind.accepts_content(&content) {
        return Ok(());
    }
    let needed = match generator {
        Generator::Text { .. } => "a text track",
        _ => "a sticker (overlay) track",
    };
    Err(Rejection::new(format!(
        "track {} is a {} lane; this generator needs {needed}",
        track.id.raw(),
        kind_name(track.kind),
    )))
}

/// Lower a wire generator. When replacing the content of an existing text
/// clip, the current style is preserved (the agent edits words, not looks).
fn lower_generator(wire: &WireGenerator, current: Option<&Generator>) -> Generator {
    match wire {
        WireGenerator::Text { content } => {
            let style = match current {
                Some(Generator::Text { style, .. }) => style.clone(),
                _ => Default::default(),
            };
            Generator::Text {
                content: content.clone(),
                style,
            }
        }
        WireGenerator::Solid { rgba } => Generator::SolidColor { rgba: *rgba },
        WireGenerator::Shape {
            shape,
            rgba,
            width,
            height,
        } => {
            let (shape_w, shape_h, corner_radius, stroke) = match current {
                Some(Generator::Shape {
                    width: w,
                    height: h,
                    corner_radius,
                    stroke,
                    ..
                }) => (
                    w.sample(0),
                    h.sample(0),
                    corner_radius.clone(),
                    stroke.clone(),
                ),
                _ => (
                    cutlass_models::SHAPE_DROP_WIDTH,
                    cutlass_models::SHAPE_DROP_HEIGHT,
                    Param::Constant(0.0),
                    None,
                ),
            };
            Generator::Shape {
                shape: match shape {
                    WireShape::Rectangle => cutlass_models::Shape::Rectangle,
                    WireShape::Ellipse => cutlass_models::Shape::Ellipse,
                },
                rgba: Param::Constant(*rgba),
                width: Param::Constant(width.unwrap_or(shape_w)),
                height: Param::Constant(height.unwrap_or(shape_h)),
                corner_radius,
                stroke,
            }
        }
    }
}

fn generated_content(clip: &Clip) -> Option<&Generator> {
    match &clip.content {
        cutlass_models::ClipSource::Generated(g) => Some(g),
        cutlass_models::ClipSource::Media { .. } => None,
    }
}

// --- seconds → ticks ---------------------------------------------------------

fn timeline_rate(project: &Project) -> Rational {
    project.timeline().frame_rate
}

/// Lower a wire speed multiplier to the engine's exact rational, snapped to
/// hundredths (2.0 → 2/1, 0.5 → 1/2, 0.333 → 33/100). CapCut's UI range.
fn rational_speed(speed: f64) -> Result<Rational, Rejection> {
    if !speed.is_finite() || !(0.05..=100.0).contains(&speed) {
        return Err(Rejection::new(format!(
            "speed must be between 0.05 and 100 (got {speed})"
        )));
    }
    let num = (speed * 100.0).round() as i32;
    let g = gcd(num, 100);
    Ok(Rational::new(num / g, 100 / g))
}

fn gcd(mut a: i32, mut b: i32) -> i32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

fn clip_param(param: WireClipParam) -> ClipParam {
    match param {
        WireClipParam::Position => ClipParam::Position,
        WireClipParam::Scale => ClipParam::Scale,
        WireClipParam::Rotation => ClipParam::Rotation,
        WireClipParam::Opacity => ClipParam::Opacity,
        WireClipParam::Volume => ClipParam::Volume,
    }
}

fn easing(easing: Option<WireEasing>) -> Easing {
    match easing {
        None | Some(WireEasing::Linear) => Easing::Linear,
        Some(WireEasing::EaseIn) => Easing::EaseIn,
        Some(WireEasing::EaseOut) => Easing::EaseOut,
        Some(WireEasing::EaseInOut) => Easing::EaseInOut,
    }
}

/// Build the typed parameter value from the wire's `value` / `position`
/// fields, rejecting the wrong shape with a message naming the right one.
fn param_value(
    param: WireClipParam,
    value: Option<f64>,
    position: Option<[f64; 2]>,
) -> Result<ParamValue, Rejection> {
    match param {
        WireClipParam::Position => position
            .map(|p| ParamValue::Vec2([p[0] as f32, p[1] as f32]))
            .ok_or_else(|| {
                Rejection::new("param 'position' needs the 'position' argument as [x, y]")
            }),
        WireClipParam::Scale
        | WireClipParam::Rotation
        | WireClipParam::Opacity
        | WireClipParam::Volume => value.map(|v| ParamValue::Scalar(v as f32)).ok_or_else(|| {
            Rejection::new(
                format!("param '{param:?}' needs the 'value' argument (a number)",).to_lowercase(),
            )
        }),
    }
}

/// A keyframe's timeline position in seconds, pre-checked against the
/// clip's extent so the model gets a message naming where the clip sits.
fn keyframe_position(
    project: &Project,
    clip: &Clip,
    seconds: f64,
) -> Result<RationalTime, Rejection> {
    let at = timeline_time(project, seconds, "at")?;
    let tl = clip.timeline;
    if at.value < tl.start.value || at.value >= tl.end_tick() {
        let rate = timeline_rate(project);
        return Err(Rejection::new(format!(
            "keyframe position {seconds:.3}s is outside clip {} ({:.3}s to {:.3}s)",
            clip.id.raw(),
            ticks_to_seconds(tl.start.value, rate),
            ticks_to_seconds(tl.end_tick(), rate),
        )));
    }
    Ok(at)
}

fn ticks_to_seconds(ticks: i64, rate: Rational) -> f64 {
    ticks as f64 * rate.seconds_per_unit()
}

fn seconds_to_ticks(seconds: f64, rate: Rational, what: &str) -> Result<i64, Rejection> {
    if !seconds.is_finite() {
        return Err(Rejection::new(format!("{what} must be a finite number")));
    }
    let ticks = seconds * f64::from(rate.num) / f64::from(rate.den);
    if !(-(2f64.powi(53))..=2f64.powi(53)).contains(&ticks) {
        return Err(Rejection::new(format!(
            "{what} of {seconds}s is out of range"
        )));
    }
    Ok(ticks.round() as i64)
}

fn require_non_negative(seconds: f64, what: &str) -> Result<(), Rejection> {
    if seconds < 0.0 {
        return Err(Rejection::new(format!(
            "{what} must not be negative (got {seconds}s)"
        )));
    }
    Ok(())
}

/// A non-negative timeline position, frame-snapped to the project rate.
fn timeline_time(project: &Project, seconds: f64, what: &str) -> Result<RationalTime, Rejection> {
    let ticks = seconds_to_ticks(seconds, timeline_rate(project), what)?;
    Ok(RationalTime::new(ticks, timeline_rate(project)))
}

/// A signed timeline delta, frame-snapped to the project rate.
fn timeline_time_signed(
    project: &Project,
    seconds: f64,
    what: &str,
) -> Result<RationalTime, Rejection> {
    let ticks = seconds_to_ticks(seconds, timeline_rate(project), what)?;
    Ok(RationalTime::new(ticks, timeline_rate(project)))
}

/// A timeline range from `start`/`duration` seconds; duration must survive
/// frame snapping with at least one frame.
fn timeline_range(project: &Project, start: f64, duration: f64) -> Result<TimeRange, Rejection> {
    require_non_negative(start, "start")?;
    if duration <= 0.0 {
        return Err(Rejection::new(format!(
            "duration must be positive (got {duration}s)"
        )));
    }
    let rate = timeline_rate(project);
    let start_ticks = seconds_to_ticks(start, rate, "start")?;
    let duration_ticks = seconds_to_ticks(duration, rate, "duration")?.max(1);
    Ok(TimeRange::at_rate(start_ticks, duration_ticks, rate))
}

/// Source range (at the media's native rate) + timeline position for
/// placing media. Pre-checks bounds so the model gets a message naming the
/// media's actual extent.
fn media_placement(
    project: &Project,
    media: &cutlass_models::MediaSource,
    source_start: f64,
    source_duration: f64,
    timeline_seconds: f64,
    timeline_what: &str,
) -> Result<(TimeRange, RationalTime), Rejection> {
    require_non_negative(source_start, "source_start")?;
    if source_duration <= 0.0 {
        return Err(Rejection::new(format!(
            "source_duration must be positive (got {source_duration}s)"
        )));
    }
    require_non_negative(timeline_seconds, timeline_what)?;

    let rate = media.frame_rate;
    let start_ticks = seconds_to_ticks(source_start, rate, "source_start")?;
    let duration_ticks = seconds_to_ticks(source_duration, rate, "source_duration")?.max(1);
    if start_ticks + duration_ticks > media.duration.value {
        return Err(Rejection::new(format!(
            "source range {:.3}s + {:.3}s exceeds media {} which is {:.3}s long",
            source_start,
            source_duration,
            media.id.raw(),
            ticks_to_seconds(media.duration.value, rate),
        )));
    }
    let source = TimeRange::at_rate(start_ticks, duration_ticks, rate);
    let at = timeline_time(project, timeline_seconds, timeline_what)?;
    Ok((source, at))
}

#[cfg(test)]
mod tests;
