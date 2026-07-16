use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Map;
use crate::clip::{
    Clip, ClipParam, ClipSource, ClipTransform, CropRect, Generator, ParamValue, Replaceable,
    SlotMedia, look_animation_combo_period_ticks, look_animation_window_ticks, split_speed_curve,
};
use crate::effects::EffectInstance;
use crate::error::ModelError;
use crate::ids::{ClipId, MediaId, ProjectId, TrackId};
use crate::look::{
    AnimationRef, AnimationSlot, AudioRole, ChromaKey, ColorAdjustments, Filter, Lut, Mask,
    StabilizeLevel, animation_spec,
};
use crate::media::MediaSource;
use crate::metadata::ProjectMetadata;
use crate::param::{Easing, Param};
use crate::schema::ProjectSchema;
use crate::time::{
    Rational, RationalTime, TimeRange, check_same_rate, resample, time_add, time_sub,
};
use crate::timeline::Timeline;
use crate::track::{Track, TrackKind};
use crate::transition::Transition;

/// Top-level container: a media pool plus exactly one [`Timeline`].
///
/// `Project` is the aggregate root and the only place that can guarantee
/// referential integrity between clips and media, so clip creation goes through
/// [`add_clip`](Project::add_clip).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// Document schema identity (version, kind, extensions).
    #[serde(
        serialize_with = "crate::schema::serialize",
        deserialize_with = "crate::schema::deserialize",
        alias = "schema_version"
    )]
    pub schema: ProjectSchema,
    pub id: ProjectId,
    pub name: String,
    pub metadata: ProjectMetadata,
    #[serde(with = "crate::serde_map")]
    media: Map<MediaId, MediaSource>,
    timeline: Timeline,
}

impl Project {
    /// Create an empty project whose timeline runs at `frame_rate`.
    pub fn new(name: impl Into<String>, frame_rate: Rational) -> Self {
        Self {
            schema: ProjectSchema::current(),
            id: ProjectId::next(),
            name: name.into(),
            metadata: ProjectMetadata::default(),
            media: Map::default(),
            timeline: Timeline::new(frame_rate),
        }
    }

    pub fn schema(&self) -> &ProjectSchema {
        &self.schema
    }

    pub fn metadata(&self) -> &ProjectMetadata {
        &self.metadata
    }

    pub fn metadata_mut(&mut self) -> &mut ProjectMetadata {
        &mut self.metadata
    }

    // --- media pool -------------------------------------------------------

    /// Add a source to the media pool. Returns its [`MediaId`].
    pub fn add_media(&mut self, media: MediaSource) -> MediaId {
        let id = media.id;
        self.media.insert(id, media);
        id
    }

    pub fn media(&self, id: MediaId) -> Option<&MediaSource> {
        self.media.get(&id)
    }

    /// Mutable pool access for state repair (missing-media relink, M0):
    /// re-pointing an entry at a re-probed file in place, keeping its id so
    /// clips stay attached. Editing flows must not mutate sources behind the
    /// timeline's back — placement math reads pool metadata.
    pub fn media_mut(&mut self, id: MediaId) -> Option<&mut MediaSource> {
        self.media.get_mut(&id)
    }

    pub fn media_iter(&self) -> impl Iterator<Item = &MediaSource> {
        self.media.values()
    }

    pub fn media_count(&self) -> usize {
        self.media.len()
    }

    /// Lookup a pool entry by filesystem path (canonical comparison when possible).
    pub fn find_media_by_path(&self, path: &Path) -> Option<MediaId> {
        self.media_iter()
            .find(|m| paths_refer_to_same_file(m.path(), path))
            .map(|m| m.id)
    }

    /// Whether any clip currently references `media_id`.
    pub fn is_media_referenced(&self, media_id: MediaId) -> bool {
        self.timeline
            .tracks_ordered()
            .flat_map(Track::clips)
            .any(|c| c.media() == Some(media_id))
    }

    /// Remove a source from the pool. Fails if any clip still references it.
    pub fn remove_media(&mut self, media_id: MediaId) -> Result<MediaSource, ModelError> {
        if self.is_media_referenced(media_id) {
            return Err(ModelError::MediaReferenced(media_id));
        }
        self.media
            .remove(&media_id)
            .ok_or(ModelError::UnknownMedia(media_id))
    }

    // --- timeline ---------------------------------------------------------

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn timeline_mut(&mut self) -> &mut Timeline {
        &mut self.timeline
    }

    /// Convenience: create and append a track, returning its [`TrackId`].
    pub fn add_track(&mut self, kind: TrackKind, name: impl Into<String>) -> TrackId {
        self.timeline.add_track(Track::new(kind, name))
    }

    /// Convenience: create a track at `order_index` in the stack (0 = bottom
    /// layer; clamped), returning its [`TrackId`].
    pub fn insert_track(
        &mut self,
        kind: TrackKind,
        name: impl Into<String>,
        order_index: usize,
    ) -> TrackId {
        self.timeline
            .insert_track(Track::new(kind, name), order_index)
    }

    /// Place a clip referencing `media_id` on `track_id`.
    ///
    /// The clip's timeline duration is resampled from the source rate to the
    /// timeline rate. Validates media/track existence, source bounds, and overlap.
    pub fn add_clip(
        &mut self,
        track_id: TrackId,
        media_id: MediaId,
        source: TimeRange,
        timeline_start: RationalTime,
    ) -> Result<ClipId, ModelError> {
        let media = self
            .media
            .get(&media_id)
            .ok_or(ModelError::UnknownMedia(media_id))?;
        let tl_rate = self.timeline.frame_rate;

        check_same_rate(source.start.rate, media.frame_rate)?;
        check_same_rate(timeline_start.rate, tl_rate)?;

        if source.is_empty() {
            return Err(ModelError::InvalidRange);
        }
        // Stills have no real material bound: one frame repeats for any
        // extent, and the pool duration is only the default placement
        // length — so any window length is legal on image media.
        if source.start.value < 0 || (!media.is_image && source.end_tick() > media.duration.value) {
            return Err(ModelError::SourceOutOfBounds);
        }

        let timeline_duration = resample(source.duration, tl_rate);
        let duration_ticks = timeline_duration.value.max(1);
        let timeline = TimeRange::at_rate(timeline_start.value, duration_ticks, tl_rate);

        let clip = Clip::from_media(media_id, source, timeline);
        self.timeline.add_clip(track_id, clip)
    }

    /// Place a generated clip on `track_id` at the given timeline range.
    pub fn add_generated(
        &mut self,
        track_id: TrackId,
        generator: Generator,
        timeline: TimeRange,
    ) -> Result<ClipId, ModelError> {
        check_same_rate(timeline.start.rate, self.timeline.frame_rate)?;
        if timeline.is_empty() {
            return Err(ModelError::InvalidRange);
        }
        let mut generator = generator;
        generator.resolve_presets()?;
        generator.validate()?;
        let clip = Clip::generated(generator, timeline);
        self.timeline.add_clip(track_id, clip)
    }

    /// Clone `clip_id` onto `to_track` at `start`, preserving every clip
    /// property except identity, placement start, and linkage.
    ///
    /// The destination is explicit: this primitive performs no placement
    /// search or ripple. Placement validation is completed before allocating
    /// the fresh [`ClipId`], avoiding allocator advances on validation errors.
    pub fn duplicate_clip(
        &mut self,
        clip_id: ClipId,
        to_track: TrackId,
        start: RationalTime,
    ) -> Result<ClipId, ModelError> {
        let timeline_rate = self.timeline.frame_rate;
        check_same_rate(start.rate, timeline_rate)?;
        if start.value < 0 {
            return Err(ModelError::InvalidRange);
        }

        let source = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let destination = self
            .timeline
            .track(to_track)
            .ok_or(ModelError::UnknownTrack(to_track))?;
        if !destination.kind.accepts_content(&source.content) {
            return Err(ModelError::IncompatibleTrackKind {
                track: to_track,
                kind: destination.kind,
            });
        }

        let mut destination_range = source.timeline;
        destination_range.start = start;
        if destination.has_overlap(destination_range, None)? {
            return Err(ModelError::Overlap(to_track));
        }

        // Clone first so every current and future content/property field rides
        // along by default. Duplication deliberately resets only these three
        // identity/placement fields; transitions live on tracks, not clips.
        let mut duplicate = source.clone();
        duplicate.id = ClipId::next();
        duplicate.timeline.start = start;
        duplicate.link = None;

        self.timeline.add_clip(to_track, duplicate)
    }

    /// Replace a generated clip's content (edit a title's text, recolor a
    /// shape, …). Errors if the clip is unknown, is media-backed, or the new
    /// generator isn't accepted by the clip's track.
    pub fn set_generator(
        &mut self,
        clip_id: ClipId,
        generator: Generator,
    ) -> Result<(), ModelError> {
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let kind = self
            .timeline
            .track(track_id)
            .ok_or(ModelError::UnknownTrack(track_id))?
            .kind;
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        // Media clips have no generator to replace; reject rather than convert.
        if !clip.is_generated() {
            return Err(ModelError::IncompatibleTrackKind {
                track: track_id,
                kind,
            });
        }
        let mut generator = generator;
        generator.resolve_presets()?;
        generator.validate()?;
        let content = ClipSource::Generated(generator);
        if !kind.accepts_content(&content) {
            return Err(ModelError::IncompatibleTrackKind {
                track: track_id,
                kind,
            });
        }
        self.timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?
            .content = content;
        Ok(())
    }

    /// Replace `clip_id`'s content with a trimmed window of `media_id`, keeping
    /// its timeline placement, speed, transform, and effects. This is the
    /// primitive behind filling a template slot and re-picking a filled slot's
    /// in-point (CapCut "tap clip → adjust which part plays"). Validates media
    /// existence, that the track accepts media content, the source rate, and —
    /// for non-image media — that the source window lies within the source.
    pub fn set_clip_media(
        &mut self,
        clip_id: ClipId,
        media_id: MediaId,
        source: TimeRange,
    ) -> Result<(), ModelError> {
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let kind = self
            .timeline
            .track(track_id)
            .ok_or(ModelError::UnknownTrack(track_id))?
            .kind;
        if self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?
            .freeze_frame
        {
            return Err(ModelError::InvalidParam(
                "cannot replace the media of a freeze-frame clip".into(),
            ));
        }
        let media = self
            .media
            .get(&media_id)
            .ok_or(ModelError::UnknownMedia(media_id))?;
        check_same_rate(source.start.rate, media.frame_rate)?;
        if source.is_empty() {
            return Err(ModelError::InvalidRange);
        }
        // Stills repeat one frame for any window; real media must contain the
        // requested span.
        if source.start.value < 0 || (!media.is_image && source.end_tick() > media.duration.value) {
            return Err(ModelError::SourceOutOfBounds);
        }
        let content = ClipSource::Media {
            media: media_id,
            source,
        };
        if !kind.accepts_content(&content) {
            return Err(ModelError::IncompatibleTrackKind {
                track: track_id,
                kind,
            });
        }
        self.timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?
            .content = content;
        Ok(())
    }

    /// Mark (or, with `None`, unmark) a clip as a CapCut-style replaceable
    /// template slot. The marker is metadata only — it does not change what
    /// renders — and is what [`Template`](crate::Template) scans to build its
    /// list of fillable slots.
    ///
    /// Marking validates eagerly (mirroring [`set_text_editable`]
    /// (Self::set_text_editable)) so authoring mistakes surface here, not at
    /// apply time with a misleading error: the clip must be a media clip
    /// (generated content cannot be refilled), and the slot's `accepts` must
    /// match the lane — visual restrictions belong on video tracks,
    /// [`SlotMedia::AudioOnly`] on audio tracks. Errors if the clip is
    /// unknown. Unmarking never fails for known clips.
    pub fn set_replaceable(
        &mut self,
        clip_id: ClipId,
        replaceable: Option<Replaceable>,
    ) -> Result<(), ModelError> {
        if let Some(marker) = &replaceable {
            let track_id = self
                .timeline
                .track_of(clip_id)
                .ok_or(ModelError::UnknownClip(clip_id))?;
            let kind = self
                .timeline
                .track(track_id)
                .ok_or(ModelError::UnknownTrack(track_id))?
                .kind;
            let clip = self
                .timeline
                .clip(clip_id)
                .ok_or(ModelError::UnknownClip(clip_id))?;
            if clip.media().is_none() {
                return Err(ModelError::InvalidParam(
                    "replaceable slots apply only to media clips".into(),
                ));
            }
            let want = if marker.accepts == SlotMedia::AudioOnly {
                TrackKind::Audio
            } else {
                TrackKind::Video
            };
            if kind != want {
                return Err(ModelError::InvalidParam(format!(
                    "a {:?} slot cannot sit on a {kind:?} track",
                    marker.accepts
                )));
            }
        }
        self.timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?
            .replaceable = replaceable;
        Ok(())
    }

    /// Mark a text clip's content as user-editable when the project is used as
    /// a template (the text keeps its style and animation). Errors if the clip
    /// is unknown or is not a [`Generator::Text`] clip.
    pub fn set_text_editable(&mut self, clip_id: ClipId, editable: bool) -> Result<(), ModelError> {
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if !matches!(clip.content, ClipSource::Generated(Generator::Text { .. })) {
            return Err(ModelError::InvalidParam(
                "text editability applies only to text generator clips".into(),
            ));
        }
        clip.text_editable = editable;
        Ok(())
    }

    /// Set a clip's spatial transform (preview move/scale/rotate, inspector
    /// numerics). Errors if the clip is unknown, sits on an audio track
    /// (nothing to place), or the transform is invalid (non-finite, scale
    /// ≤ 0, opacity outside 0..=1).
    ///
    /// `at` composes the edit with animation CapCut-style: `Some(timeline
    /// tick)` writes a keyframe at that position on properties that already
    /// have keyframes (constants stay constant); `None` flattens every
    /// property to a constant, dropping keyframes. Never-animated clips
    /// behave identically either way.
    pub fn set_transform(
        &mut self,
        clip_id: ClipId,
        transform: ClipTransform,
        at: Option<RationalTime>,
    ) -> Result<(), ModelError> {
        transform.validate()?;
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let kind = self
            .timeline
            .track(track_id)
            .ok_or(ModelError::UnknownTrack(track_id))?
            .kind;
        if !kind.is_visual() {
            return Err(ModelError::IncompatibleTrackKind {
                track: track_id,
                kind,
            });
        }
        if let Some(at) = at {
            check_same_rate(at.rate, self.timeline.frame_rate)?;
        }
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        match at {
            Some(at) => {
                let tick = clip.animation_tick(at.value);
                clip.transform.compose_at(transform, tick);
            }
            None => clip.transform.set_constant(transform),
        }
        Ok(())
    }

    /// Shared precondition for parameter edits: the clip exists on a visual
    /// track. Returns the track kind error otherwise (audio has no canvas
    /// placement to animate).
    fn check_param_target(&self, clip_id: ClipId) -> Result<(), ModelError> {
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let kind = self
            .timeline
            .track(track_id)
            .ok_or(ModelError::UnknownTrack(track_id))?
            .kind;
        if !kind.is_visual() {
            return Err(ModelError::IncompatibleTrackKind {
                track: track_id,
                kind,
            });
        }
        Ok(())
    }

    /// Precondition for volume-envelope edits (M8): the clip exists and is
    /// media-backed (generators have nothing to hear). Mirrors
    /// [`Self::set_clip_audio`]'s target rule — volume rides any media clip,
    /// since linkage lands the audible half on an audio lane.
    fn check_audio_param_target(&self, clip_id: ClipId) -> Result<(), ModelError> {
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if clip.is_generated() {
            return Err(ModelError::InvalidParam(
                "volume requires a media-backed clip".into(),
            ));
        }
        Ok(())
    }

    /// Convert an absolute timeline position to a clip-relative animation
    /// tick, rejecting positions outside the clip (a keyframe must sit on
    /// the clip it animates).
    fn keyframe_tick(&self, clip_id: ClipId, at: RationalTime) -> Result<i64, ModelError> {
        check_same_rate(at.rate, self.timeline.frame_rate)?;
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if !clip.timeline.contains(at)? {
            return Err(ModelError::InvalidParam(format!(
                "keyframe position {} is outside clip {clip_id}",
                at.value
            )));
        }
        Ok(at.value - clip.timeline.start.value)
    }

    /// Insert or replace a keyframe on one animatable clip property. `at` is
    /// an absolute timeline position and must fall inside the clip.
    pub fn set_param_keyframe(
        &mut self,
        clip_id: ClipId,
        param: ClipParam,
        at: RationalTime,
        value: ParamValue,
        easing: Easing,
    ) -> Result<(), ModelError> {
        // Volume (M8) is an audio property, not a transform: validate the
        // gain range and an audio-capable target, then write to the envelope.
        if param == ClipParam::Volume {
            easing.validate()?;
            let v = scalar_param(value)?;
            crate::clip::validate_volume(v)?;
            self.check_audio_param_target(clip_id)?;
            let tick = self.keyframe_tick(clip_id, at)?;
            let clip = self
                .timeline
                .clip_mut(clip_id)
                .ok_or(ModelError::UnknownClip(clip_id))?;
            clip.volume.set_keyframe(tick, v, easing);
            return Ok(());
        }
        self.check_param_target(clip_id)?;
        let tick = self.keyframe_tick(clip_id, at)?;
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        match param {
            ClipParam::Effect { effect, param } => {
                let v = scalar_param(value)?;
                effect_mut(clip, effect)?.set_param_keyframe(param as usize, tick, v, easing)
            }
            ClipParam::Shape { param } => {
                generator_mut(clip)?.set_shape_param_keyframe(param, tick, value, easing)
            }
            _ => clip
                .transform
                .set_param_keyframe(param, tick, value, easing),
        }
    }

    /// Remove the keyframe at exactly `at` (absolute timeline position) on
    /// one property. Errors when no keyframe sits there.
    pub fn remove_param_keyframe(
        &mut self,
        clip_id: ClipId,
        param: ClipParam,
        at: RationalTime,
    ) -> Result<(), ModelError> {
        if param == ClipParam::Volume {
            self.check_audio_param_target(clip_id)?;
            let tick = self.keyframe_tick(clip_id, at)?;
            let clip = self
                .timeline
                .clip_mut(clip_id)
                .ok_or(ModelError::UnknownClip(clip_id))?;
            return if clip.volume.remove_keyframe(tick) {
                Ok(())
            } else {
                Err(ModelError::InvalidParam(format!(
                    "no volume keyframe at {} to remove",
                    at.value
                )))
            };
        }
        self.check_param_target(clip_id)?;
        let tick = self.keyframe_tick(clip_id, at)?;
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        match param {
            ClipParam::Effect { effect, param } => {
                effect_mut(clip, effect)?.remove_param_keyframe(param as usize, tick)
            }
            ClipParam::Shape { param } => {
                generator_mut(clip)?.remove_shape_param_keyframe(param, tick)
            }
            _ => clip.transform.remove_param_keyframe(param, tick),
        }
    }

    /// Replace one animatable property with a constant, dropping keyframes.
    pub fn set_param_constant(
        &mut self,
        clip_id: ClipId,
        param: ClipParam,
        value: ParamValue,
    ) -> Result<(), ModelError> {
        if param == ClipParam::Volume {
            let v = scalar_param(value)?;
            crate::clip::validate_volume(v)?;
            self.check_audio_param_target(clip_id)?;
            let clip = self
                .timeline
                .clip_mut(clip_id)
                .ok_or(ModelError::UnknownClip(clip_id))?;
            clip.volume.set_constant(v);
            return Ok(());
        }
        self.check_param_target(clip_id)?;
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        match param {
            ClipParam::Effect { effect, param } => {
                let v = scalar_param(value)?;
                effect_mut(clip, effect)?.set_param_constant(param as usize, v)
            }
            ClipParam::Shape { param } => {
                generator_mut(clip)?.set_shape_param_constant(param, value)
            }
            _ => clip.transform.set_param_constant(param, value),
        }
    }

    /// Append an effect (M4) to a visual clip's chain; the id must exist in
    /// the catalog. Returns the new effect's index. Rejected on audio clips.
    pub fn add_effect(&mut self, clip_id: ClipId, effect_id: &str) -> Result<usize, ModelError> {
        let instance = EffectInstance::new(effect_id);
        // Reject unknown ids up front (validate also covers an empty chain).
        instance.validate()?;
        self.check_param_target(clip_id)?;
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        clip.effects.push(instance);
        Ok(clip.effects.len() - 1)
    }

    /// Remove the effect at `index` from a clip's chain.
    pub fn remove_effect(&mut self, clip_id: ClipId, index: usize) -> Result<(), ModelError> {
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if index >= clip.effects.len() {
            return Err(ModelError::InvalidParam(format!(
                "effect index {index} out of range"
            )));
        }
        clip.effects.remove(index);
        Ok(())
    }

    /// Move one effect within a clip's chain. Both indices address the chain
    /// before the move; `to_index` is the effect's final index after removal
    /// and insertion. Moving an effect to its current index is a valid no-op.
    pub fn move_effect(
        &mut self,
        clip_id: ClipId,
        from_index: usize,
        to_index: usize,
    ) -> Result<(), ModelError> {
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let len = clip.effects.len();
        if from_index >= len {
            return Err(ModelError::InvalidParam(format!(
                "effect from index {from_index} out of range for chain length {len}"
            )));
        }
        if to_index >= len {
            return Err(ModelError::InvalidParam(format!(
                "effect to index {to_index} out of range for chain length {len}"
            )));
        }
        if from_index == to_index {
            return Ok(());
        }

        let effect = clip.effects.remove(from_index);
        clip.effects.insert(to_index, effect);
        Ok(())
    }

    /// Set one effect parameter to a constant (the non-animated quick edit;
    /// keyframes go through [`Self::set_param_keyframe`] with
    /// [`ClipParam::Effect`]).
    pub fn set_effect_param(
        &mut self,
        clip_id: ClipId,
        index: usize,
        param: usize,
        value: f32,
    ) -> Result<(), ModelError> {
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        effect_mut(clip, index as u32)?.set_param_constant(param, value)
    }

    // --- transitions (M4) -------------------------------------------------

    /// Add (or replace) a transition at the junction where `left` abuts the
    /// next clip on its track. The catalog id must exist and `left` must abut
    /// a following clip. Uses the default window length.
    pub fn add_transition(&mut self, left: ClipId, transition_id: &str) -> Result<(), ModelError> {
        if crate::transition::transition_spec(transition_id).is_none() {
            return Err(ModelError::InvalidParam(format!(
                "unknown transition '{transition_id}'"
            )));
        }
        let track_id = self
            .timeline
            .track_of(left)
            .ok_or(ModelError::UnknownClip(left))?;
        let kind = self
            .timeline
            .track(track_id)
            .ok_or(ModelError::UnknownClip(left))?
            .kind;
        if !kind.supports_transitions() {
            return Err(ModelError::InvalidParam(format!(
                "transitions aren't supported on {kind:?} lanes"
            )));
        }
        let right = self.right_neighbor(track_id, left).ok_or_else(|| {
            ModelError::InvalidParam("clip has no abutting clip to its right".into())
        })?;
        let transition = Transition::new(
            left,
            right,
            transition_id,
            crate::transition::DEFAULT_TRANSITION_TICKS,
        );
        self.timeline
            .track_mut(track_id)
            .ok_or(ModelError::UnknownClip(left))?
            .upsert_transition(transition);
        Ok(())
    }

    /// Remove the transition at the `left` junction. Errors if none exists.
    pub fn remove_transition(&mut self, left: ClipId) -> Result<(), ModelError> {
        let track_id = self
            .timeline
            .track_of(left)
            .ok_or(ModelError::UnknownClip(left))?;
        let removed = self
            .timeline
            .track_mut(track_id)
            .and_then(|t| t.remove_transition(left));
        if removed.is_none() {
            return Err(ModelError::InvalidParam(
                "clip has no transition at its right junction".into(),
            ));
        }
        Ok(())
    }

    /// Set the window length (timeline ticks) of an existing transition.
    pub fn set_transition_duration(
        &mut self,
        left: ClipId,
        duration: i64,
    ) -> Result<(), ModelError> {
        let track_id = self
            .timeline
            .track_of(left)
            .ok_or(ModelError::UnknownClip(left))?;
        let transition = self
            .timeline
            .track_mut(track_id)
            .and_then(|t| t.transition_at_mut(left))
            .ok_or_else(|| {
                ModelError::InvalidParam("clip has no transition at its right junction".into())
            })?;
        transition.duration = duration.max(1);
        Ok(())
    }

    /// The clip on `track` whose start abuts the end of `left`, if any.
    fn right_neighbor(&self, track_id: TrackId, left: ClipId) -> Option<ClipId> {
        let track = self.timeline.track(track_id)?;
        let left_end = track.clip(left)?.timeline.end_tick();
        track
            .clips()
            .find(|c| c.id != left && c.timeline.start.value == left_end)
            .map(|c| c.id)
    }

    /// Whether any track carries a transition (cheap guard for prune).
    pub fn has_transitions(&self) -> bool {
        self.timeline
            .tracks_ordered()
            .any(|t| !t.transitions().is_empty())
    }

    /// Snapshot every track's transitions (for undo of structural edits that
    /// prune dead junctions).
    pub fn transitions_snapshot(&self) -> Vec<(TrackId, Vec<Transition>)> {
        self.timeline
            .tracks_ordered()
            .map(|t| (t.id, t.transitions().to_vec()))
            .collect()
    }

    /// Restore a [`Self::transitions_snapshot`] across tracks.
    pub fn restore_transitions(&mut self, snapshot: Vec<(TrackId, Vec<Transition>)>) {
        for (track_id, transitions) in snapshot {
            if let Some(track) = self.timeline.track_mut(track_id) {
                track.set_transitions(transitions);
            }
        }
    }

    /// Drop transitions whose junction no longer abuts, across all tracks.
    /// Returns whether anything was pruned.
    pub fn prune_dead_transitions(&mut self) -> bool {
        let track_ids: Vec<TrackId> = self.timeline.tracks_ordered().map(|t| t.id).collect();
        let mut pruned = false;
        for id in track_ids {
            if let Some(track) = self.timeline.track_mut(id) {
                pruned |= track.prune_dead_transitions();
            }
        }
        pruned
    }

    /// Find a clip by ID anywhere on the timeline (O(1)).
    pub fn clip(&self, clip_id: ClipId) -> Option<&Clip> {
        self.timeline.clip(clip_id)
    }

    // --- editing primitives ----------------------------------------------

    pub fn remove_clip(&mut self, clip_id: ClipId) -> Option<Clip> {
        self.timeline.remove_clip(clip_id)
    }

    /// Split the clip at timeline position `at` into two abutting clips.
    pub fn split_clip(&mut self, clip_id: ClipId, at: RationalTime) -> Result<ClipId, ModelError> {
        let clip = self
            .timeline
            .clip(clip_id)
            .cloned()
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let tl = clip.timeline;
        let tl_rate = self.timeline.frame_rate;
        check_same_rate(at.rate, tl_rate)?;

        if at.value <= tl.start.value || at.value >= tl.end_tick() {
            return Err(ModelError::InvalidRange);
        }

        let left_tl = TimeRange::at_rate(tl.start.value, at.value - tl.start.value, tl_rate);
        let right_tl = TimeRange::at_rate(at.value, tl.end_tick() - at.value, tl_rate);
        validate_split_render_continuity(
            &clip,
            left_tl.duration.value,
            right_tl.duration.value,
            tl_rate,
        )?;
        let split_fraction = left_tl.duration.value as f64 / tl.duration.value as f64;
        let (left_speed_curve, right_speed_curve) =
            split_speed_curve(&clip.speed_curve, split_fraction)?;

        let new_left_source = match clip.content.clone() {
            ClipSource::Media { media, source } => {
                let media_fps = self
                    .media
                    .get(&media)
                    .ok_or(ModelError::UnknownMedia(media))?
                    .frame_rate;
                if clip.freeze_frame {
                    let held = TimeRange::at_rate(source.start.value, 1, media_fps);
                    Some((held, held))
                } else {
                    if source.duration.value < 2 {
                        return Err(ModelError::InvalidRange);
                    }
                    // Use the original clip's actual source positions on both
                    // sides of the cut. This includes exact rational base
                    // speed, integrated speed ramps, mixed source/timeline
                    // rates, and reversal. If adjacent timeline frames resolve
                    // to the same source frame, disjoint non-empty source
                    // windows cannot preserve both halves, reject the cut.
                    let boundary = clip.source_time_at(at)?.ok_or(ModelError::InvalidRange)?;
                    let previous_at = RationalTime::new(
                        at.value.checked_sub(1).ok_or(ModelError::TimeOverflow)?,
                        tl_rate,
                    );
                    let previous = clip
                        .source_time_at(previous_at)?
                        .ok_or(ModelError::InvalidRange)?;
                    let source_last = source.start.value + source.duration.value - 1;
                    let left_src_dur = if clip.reversed {
                        if previous.value <= boundary.value {
                            return Err(ModelError::InvalidRange);
                        }
                        source_last - boundary.value
                    } else {
                        if previous.value >= boundary.value {
                            return Err(ModelError::InvalidRange);
                        }
                        boundary.value - source.start.value
                    };
                    if left_src_dur <= 0 || left_src_dur >= source.duration.value {
                        return Err(ModelError::InvalidRange);
                    }
                    // A reversed clip plays its window backward: the timeline's
                    // left half shows the source window's TOP, so the split
                    // hands the window bottom to the right clip.
                    let (left_src_start, right_src_start) = if clip.reversed {
                        (
                            source.start.value + source.duration.value - left_src_dur,
                            source.start.value,
                        )
                    } else {
                        (source.start.value, source.start.value + left_src_dur)
                    };
                    let left_source = TimeRange::at_rate(left_src_start, left_src_dur, media_fps);
                    let right_source = TimeRange::at_rate(
                        right_src_start,
                        source.duration.value - left_src_dur,
                        media_fps,
                    );
                    Some((left_source, right_source))
                }
            }
            ClipSource::Generated(_) => None,
        };

        // Clone first so newly-added serde-default fields automatically survive
        // future splits. Only split identity/placement/linkage and the fields
        // whose domains are anchored to clip time are changed below.
        let mut new_clip = clip.clone();
        new_clip.id = ClipId::next();
        new_clip.timeline = right_tl;
        // Existing policy: the shell decides whether split tails should form a
        // new link group (linked multi-clip splits relink them explicitly).
        new_clip.link = None;
        if let (Some((_, right_source)), ClipSource::Media { source, .. }) =
            (new_left_source, &mut new_clip.content)
        {
            *source = right_source;
        }
        // Ordinary animation keyframes are clip-relative. Moving every tail
        // keyframe left by the split offset makes sampling tail tick `t`
        // identical to sampling original tick `split + t`.
        new_clip.shift_timeline_params(-left_tl.duration.value)?;
        new_clip.speed_curve = right_speed_curve;
        // Edge-anchored properties stay on the corresponding outer edge.
        new_clip.fade_in = 0;
        if new_clip.animation_combo.is_none() {
            new_clip.animation_in = None;
        }

        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;

        {
            let left = self
                .timeline
                .clip_mut(clip_id)
                .expect("clip existence checked above");
            left.timeline = left_tl;
            // The tail owns properties anchored to the original right edge.
            left.fade_out = 0;
            if left.animation_combo.is_none() {
                left.animation_out = None;
            }
            if let (Some((left_source, _)), ClipSource::Media { source, .. }) =
                (new_left_source, &mut left.content)
            {
                *source = left_source;
            }
            left.speed_curve = left_speed_curve;
        }
        self.timeline.add_clip(track_id, new_clip)
    }

    /// Set the clip's timeline placement to `new_timeline` (trim/extend).
    pub fn trim_clip(
        &mut self,
        clip_id: ClipId,
        new_timeline: TimeRange,
    ) -> Result<(), ModelError> {
        if new_timeline.is_empty() {
            return Err(ModelError::InvalidRange);
        }
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let old_tl = clip.timeline;
        let tl_rate = self.timeline.frame_rate;
        check_same_rate(new_timeline.start.rate, tl_rate)?;

        if self
            .timeline
            .track(track_id)
            .expect("clip is on a track")
            .has_overlap(new_timeline, Some(clip_id))?
        {
            return Err(ModelError::Overlap(track_id));
        }

        let new_source = match clip.content.clone() {
            ClipSource::Media { media: _, source } if clip.freeze_frame => {
                Some(TimeRange::at_rate(source.start.value, 1, source.start.rate))
            }
            ClipSource::Media { media, source } => {
                let media = self
                    .media
                    .get(&media)
                    .ok_or(ModelError::UnknownMedia(media))?;
                // Source ticks consumed per timeline tick scale with the
                // clip's speed (1:1 for never-retimed clips).
                let head_delta = clip.scale_by_speed(
                    resample(
                        RationalTime::new(new_timeline.start.value - old_tl.start.value, tl_rate),
                        media.frame_rate,
                    )
                    .value,
                );
                let new_src_dur = clip
                    .scale_by_speed(resample(new_timeline.duration, media.frame_rate).value)
                    .max(1);
                // A reversed clip plays its window backward, so the
                // timeline head shows the window's END: a head trim drops
                // source from the top, a tail trim from the bottom —
                // mirror-image of the forward case.
                let new_src_start = if clip.reversed {
                    source.start.value + source.duration.value - new_src_dur - head_delta
                } else {
                    source.start.value + head_delta
                };
                // Stills extend freely past the pool's default 5s window —
                // the one frame repeats and decode ignores the window, so
                // the source range is duration bookkeeping only. Clamp the
                // start to 0 so extensions stay canonical.
                if media.is_image {
                    Some(TimeRange::at_rate(
                        new_src_start.max(0),
                        new_src_dur,
                        media.frame_rate,
                    ))
                } else {
                    if new_src_start < 0 || new_src_start + new_src_dur > media.duration.value {
                        return Err(ModelError::SourceOutOfBounds);
                    }
                    Some(TimeRange::at_rate(
                        new_src_start,
                        new_src_dur,
                        media.frame_rate,
                    ))
                }
            }
            ClipSource::Generated(_) => None,
        };

        let clip = self
            .timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above");
        clip.timeline = new_timeline;
        if let (Some(src), ClipSource::Media { source, .. }) = (new_source, &mut clip.content) {
            *source = src;
        }
        Ok(())
    }

    /// Retime a media clip (CapCut speed, M1): keep its timeline start and
    /// source window, set `speed`/`reversed`, and re-derive the timeline
    /// duration (source duration ÷ speed — faster clips occupy less
    /// timeline). Rejected on generated clips (no source to retime), on
    /// non-positive speeds, and when the retimed extent would overlap a
    /// neighbor.
    pub fn set_clip_speed(
        &mut self,
        clip_id: ClipId,
        speed: Rational,
        reversed: bool,
    ) -> Result<(), ModelError> {
        if speed.num <= 0 || speed.den <= 0 {
            return Err(ModelError::InvalidParam("speed must be positive".into()));
        }
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if clip.freeze_frame {
            return Err(ModelError::InvalidParam(
                "freeze-frame clips cannot be retimed".into(),
            ));
        }
        let Some(source) = clip.source_range() else {
            return Err(ModelError::InvalidParam(
                "speed requires a media-backed clip".into(),
            ));
        };
        let tl_rate = self.timeline.frame_rate;
        let src_dur_tl = resample(source.duration, tl_rate).value;
        // Faster average ⇒ less timeline. A flat ramp keeps the exact integer
        // path (no f64 drift); any active ramp folds in its average.
        let new_dur = retimed_duration(
            src_dur_tl,
            speed,
            clip.speed_curve_average(),
            clip.has_speed_curve(),
        );
        let new_timeline = TimeRange::at_rate(clip.timeline.start.value, new_dur, tl_rate);

        if self
            .timeline
            .track(track_id)
            .expect("clip is on a track")
            .has_overlap(new_timeline, Some(clip_id))?
        {
            return Err(ModelError::Overlap(track_id));
        }

        let clip = self
            .timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above");
        clip.speed = speed;
        clip.reversed = reversed;
        clip.timeline = new_timeline;
        Ok(())
    }

    /// Set (or clear) a media clip's playback-rate ramp (CapCut speed curves,
    /// M2): keep its timeline start, base `speed`, and source window; store
    /// the normalized `curve` (`None` clears it to a flat unit ramp); and
    /// re-derive the timeline duration from `source ÷ (base_speed ×
    /// average_curve)`. Rejected on generated clips, malformed curves, and
    /// when the retimed extent would overlap a neighbor.
    pub fn set_clip_speed_curve(
        &mut self,
        clip_id: ClipId,
        curve: Option<Param<f32>>,
    ) -> Result<(), ModelError> {
        if let Some(curve) = &curve {
            crate::clip::validate_speed_curve(curve)?;
        }
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if clip.freeze_frame {
            return Err(ModelError::InvalidParam(
                "freeze-frame clips cannot use speed curves".into(),
            ));
        }
        let Some(source) = clip.source_range() else {
            return Err(ModelError::InvalidParam(
                "speed ramps require a media-backed clip".into(),
            ));
        };
        let new_curve = curve.unwrap_or(Param::Constant(1.0));
        let has_curve = !matches!(&new_curve, Param::Constant(v) if *v == 1.0);
        let average = match &new_curve {
            Param::Constant(v) => f64::from(*v),
            Param::Keyframed { .. } => {
                // Reuse the clip's integral over the candidate curve.
                let mut probe = clip.clone();
                probe.speed_curve = new_curve.clone();
                probe.speed_curve_average()
            }
        };

        let tl_rate = self.timeline.frame_rate;
        let src_dur_tl = resample(source.duration, tl_rate).value;
        let new_dur = retimed_duration(src_dur_tl, clip.speed, average, has_curve);
        let new_timeline = TimeRange::at_rate(clip.timeline.start.value, new_dur, tl_rate);

        if self
            .timeline
            .track(track_id)
            .expect("clip is on a track")
            .has_overlap(new_timeline, Some(clip_id))?
        {
            return Err(ModelError::Overlap(track_id));
        }

        let clip = self
            .timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above");
        clip.speed_curve = new_curve;
        clip.timeline = new_timeline;
        Ok(())
    }

    /// Toggle whether a retimed media clip preserves pitch while it plays
    /// (CapCut's "pitch" switch, M8 Phase 3): `true` time-stretches so the
    /// audio keeps its pitch, `false` lets pitch ride the speed ("chipmunk").
    /// Pure audio property — it changes no duration, so there is no overlap
    /// check. Rejected on generated clips (nothing to hear).
    pub fn set_clip_pitch(
        &mut self,
        clip_id: ClipId,
        preserve_pitch: bool,
    ) -> Result<(), ModelError> {
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if clip.source_range().is_none() {
            return Err(ModelError::InvalidParam(
                "pitch lock requires a media-backed clip".into(),
            ));
        }
        clip.preserve_pitch = preserve_pitch;
        Ok(())
    }

    /// Toggle noise reduction on a media clip (CapCut "Reduce noise", M8
    /// Phase 5), returning the previous flag for the inverse. The mixers run
    /// the clip's audio through RNNoise when set. Rejected on generated clips
    /// (no source audio to clean).
    pub fn set_clip_denoise(&mut self, clip_id: ClipId, denoise: bool) -> Result<bool, ModelError> {
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if clip.source_range().is_none() {
            return Err(ModelError::InvalidParam(
                "noise reduction requires a media-backed clip".into(),
            ));
        }
        Ok(std::mem::replace(&mut clip.denoise, denoise))
    }

    /// Set a media clip's audio mix (CapCut volume + fades): `volume` is
    /// `Some` to set a flat gain (`0` mutes, `1` unchanged, up to
    /// [`crate::MAX_CLIP_VOLUME`]× boost), overwriting any M8 envelope
    /// (CapCut's basic slider), or `None` to keep the current gain and only
    /// update the fades — so a fade edit never flattens an envelope. Fades
    /// are linear in/out durations at the timeline rate. Rejected on
    /// generated clips (nothing to hear), out-of-range volume, negative
    /// fades, and fades longer than the clip.
    pub fn set_clip_audio(
        &mut self,
        clip_id: ClipId,
        volume: Option<f32>,
        fade_in: RationalTime,
        fade_out: RationalTime,
    ) -> Result<(), ModelError> {
        if let Some(volume) = volume {
            crate::clip::validate_volume(volume)?;
        }
        let tl_rate = self.timeline.frame_rate;
        check_same_rate(fade_in.rate, tl_rate)?;
        check_same_rate(fade_out.rate, tl_rate)?;

        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if clip.is_generated() {
            return Err(ModelError::InvalidParam(
                "volume requires a media-backed clip".into(),
            ));
        }
        let duration = clip.timeline.duration.value;
        for (name, fade) in [("fade_in", fade_in.value), ("fade_out", fade_out.value)] {
            if fade < 0 {
                return Err(ModelError::InvalidParam(format!("{name} must be ≥ 0")));
            }
            if fade > duration {
                return Err(ModelError::InvalidParam(format!(
                    "{name} ({fade} ticks) is longer than the clip ({duration} ticks)"
                )));
            }
        }

        let clip = self
            .timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above");
        // `Some` is the basic volume slider: a flat level that flattens any
        // envelope (CapCut). `None` keeps the gain (constant or envelope) and
        // touches only the fades, so a fade edit never destroys automation;
        // envelopes are otherwise drawn through the volume keyframe commands
        // (`ClipParam::Volume`).
        if let Some(volume) = volume {
            clip.volume = Param::Constant(volume);
        }
        clip.fade_in = fade_in.value;
        clip.fade_out = fade_out.value;
        Ok(())
    }

    /// Set a clip's framing (CapCut crop, M1): the normalized kept region
    /// plus horizontal/vertical mirroring. Visual clips only — audio has no
    /// frame to crop. Rejected on a degenerate or out-of-frame crop rect.
    pub fn set_clip_crop(
        &mut self,
        clip_id: ClipId,
        crop: CropRect,
        flip_h: bool,
        flip_v: bool,
    ) -> Result<(), ModelError> {
        crop.validate()?;
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let kind = self
            .timeline
            .track(track_id)
            .ok_or(ModelError::UnknownTrack(track_id))?
            .kind;
        if !kind.is_visual() {
            return Err(ModelError::IncompatibleTrackKind {
                track: track_id,
                kind,
            });
        }
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        clip.crop = crop;
        clip.flip_h = flip_h;
        clip.flip_v = flip_v;
        Ok(())
    }

    /// Replace a clip's detected beat markers (M8 Phase 6), returning the
    /// previous list for the inverse. Beats are source ticks at the media
    /// frame rate; this only stores what detection (or a clear) produced —
    /// the engine owns the analysis. Media clips only (generated content has
    /// no audio to analyze). Stored sorted + de-duplicated and clamped to the
    /// source window so a stale list can't snap to phantom positions.
    pub fn set_clip_beats(
        &mut self,
        clip_id: ClipId,
        beats: Vec<i64>,
    ) -> Result<Vec<i64>, ModelError> {
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let source = clip
            .source_range()
            .ok_or_else(|| ModelError::InvalidParam("beats require a media-backed clip".into()))?;
        let (lo, hi) = (
            source.start.value,
            source.start.value + source.duration.value,
        );
        let mut beats: Vec<i64> = beats
            .into_iter()
            .filter(|&b| (lo..hi).contains(&b))
            .collect();
        beats.sort_unstable();
        beats.dedup();

        let clip = self
            .timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above");
        Ok(std::mem::replace(&mut clip.beats, beats))
    }

    // --- Phase I look properties (persisted + validated, render-neutral) ----

    /// The kind of the track hosting `clip_id`, or the unknown-clip error.
    fn track_kind_of(&self, clip_id: ClipId) -> Result<(TrackId, TrackKind), ModelError> {
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let kind = self
            .timeline
            .track(track_id)
            .ok_or(ModelError::UnknownTrack(track_id))?
            .kind;
        Ok((track_id, kind))
    }

    /// Guard for edits that only make sense where pixels show.
    fn require_visual(&self, clip_id: ClipId, what: &str) -> Result<(), ModelError> {
        let (track_id, kind) = self.track_kind_of(clip_id)?;
        if !kind.is_visual() {
            return Err(ModelError::IncompatibleTrackKind {
                track: track_id,
                kind,
            });
        }
        let _ = what;
        Ok(())
    }

    /// Guard for edits that need decoded source pixels (mask, chroma, …):
    /// a media-backed clip on a visual lane.
    fn require_visual_media(&self, clip_id: ClipId, what: &str) -> Result<(), ModelError> {
        self.require_visual(clip_id, what)?;
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if clip.is_generated() {
            return Err(ModelError::InvalidParam(format!(
                "{what} requires a media-backed clip"
            )));
        }
        Ok(())
    }

    /// Set (or with `None` clear) a clip's shaped alpha mask (CapCut mask,
    /// Phase I — persisted now, rendered later). Media-backed visual clips
    /// only; the mask itself is validated (feather range).
    pub fn set_clip_mask(&mut self, clip_id: ClipId, mask: Option<Mask>) -> Result<(), ModelError> {
        if let Some(mask) = &mask {
            mask.validate()?;
        }
        self.require_visual_media(clip_id, "a mask")?;
        self.timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above")
            .mask = mask;
        Ok(())
    }

    /// Set (or clear) chroma keying (CapCut chroma key, Phase I —
    /// render-neutral). Media-backed visual clips only; strength/shadow
    /// validated to `0..=1`.
    pub fn set_clip_chroma_key(
        &mut self,
        clip_id: ClipId,
        chroma: Option<ChromaKey>,
    ) -> Result<(), ModelError> {
        if let Some(chroma) = &chroma {
            chroma.validate()?;
        }
        self.require_visual_media(clip_id, "chroma key")?;
        self.timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above")
            .chroma_key = chroma;
        Ok(())
    }

    /// Set (or clear) stabilization (CapCut stabilize, Phase I —
    /// render-neutral). Media-backed *video* clips only: stills have no
    /// motion to smooth.
    pub fn set_clip_stabilize(
        &mut self,
        clip_id: ClipId,
        stabilize: Option<StabilizeLevel>,
    ) -> Result<(), ModelError> {
        self.require_visual_media(clip_id, "stabilization")?;
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        if let ClipSource::Media { media, .. } = &clip.content
            && self.media(*media).is_some_and(|m| m.is_image)
        {
            return Err(ModelError::InvalidParam(
                "stabilization requires video (stills have no motion)".into(),
            ));
        }
        self.timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above")
            .stabilize = stabilize;
        Ok(())
    }

    /// Set (or clear) a color-grade filter preset (CapCut filters, Phase I —
    /// render-neutral). Any visual clip, including `Generator::Filter` lane
    /// bars; the id must exist in the filter catalog.
    pub fn set_clip_filter(
        &mut self,
        clip_id: ClipId,
        filter: Option<Filter>,
    ) -> Result<(), ModelError> {
        if let Some(filter) = &filter {
            filter.validate()?;
        }
        self.require_visual(clip_id, "a filter")?;
        self.timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above")
            .filter = filter;
        Ok(())
    }

    /// Set (or clear) a `.cube` 3D LUT (applied after filter + adjust). Any
    /// visual clip, including `Generator::Filter` lane bars, which apply it
    /// to everything composited beneath them.
    pub fn set_clip_lut(&mut self, clip_id: ClipId, lut: Option<Lut>) -> Result<(), ModelError> {
        if let Some(lut) = &lut {
            lut.validate()?;
        }
        self.require_visual(clip_id, "a LUT")?;
        self.timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above")
            .lut = lut;
        Ok(())
    }

    /// Set a clip's manual color grade (CapCut adjust, Phase I —
    /// render-neutral). Any visual clip, including `Generator::Adjustment`
    /// lane bars; neutral values simply clear the grade.
    pub fn set_clip_adjustments(
        &mut self,
        clip_id: ClipId,
        adjust: ColorAdjustments,
    ) -> Result<(), ModelError> {
        adjust.validate()?;
        self.require_visual(clip_id, "adjustments")?;
        self.timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above")
            .adjust = adjust;
        Ok(())
    }

    /// Set (or clear) one animation slot (CapCut animation In / Out / Combo,
    /// Phase I — render-neutral). Any visual clip. The preset must exist in
    /// the catalog, match the slot, and — for `text_only` presets — sit on a
    /// text clip. CapCut exclusivity is enforced here so every platform
    /// agrees: setting In or Out clears a combo, setting a combo clears both
    /// sides.
    pub fn set_clip_animation(
        &mut self,
        clip_id: ClipId,
        slot: AnimationSlot,
        animation: Option<AnimationRef>,
    ) -> Result<(), ModelError> {
        self.require_visual(clip_id, "animations")?;
        if let Some(animation) = &animation {
            let spec = animation_spec(&animation.id).ok_or_else(|| {
                ModelError::InvalidParam(format!("unknown animation '{}'", animation.id))
            })?;
            if spec.slot != slot {
                return Err(ModelError::InvalidParam(format!(
                    "animation '{}' does not fit that slot",
                    animation.id
                )));
            }
            if spec.text_only {
                let clip = self
                    .timeline
                    .clip(clip_id)
                    .ok_or(ModelError::UnknownClip(clip_id))?;
                if !matches!(clip.content, ClipSource::Generated(Generator::Text { .. })) {
                    return Err(ModelError::InvalidParam(format!(
                        "animation '{}' is a text preset",
                        animation.id
                    )));
                }
            }
        }
        let clip = self
            .timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above");
        match slot {
            AnimationSlot::In => {
                clip.animation_in = animation;
                if clip.animation_in.is_some() {
                    clip.animation_combo = None;
                }
            }
            AnimationSlot::Out => {
                clip.animation_out = animation;
                if clip.animation_out.is_some() {
                    clip.animation_combo = None;
                }
            }
            AnimationSlot::Combo => {
                clip.animation_combo = animation;
                if clip.animation_combo.is_some() {
                    clip.animation_in = None;
                    clip.animation_out = None;
                }
            }
        }
        Ok(())
    }

    /// Tag (or untag) what an audio-lane clip *is* (music / sound FX /
    /// voiceover / extracted, Phase I). Clips on audio tracks only.
    pub fn set_clip_audio_role(
        &mut self,
        clip_id: ClipId,
        role: Option<AudioRole>,
    ) -> Result<(), ModelError> {
        let (track_id, kind) = self.track_kind_of(clip_id)?;
        if kind != TrackKind::Audio {
            return Err(ModelError::IncompatibleTrackKind {
                track: track_id,
                kind,
            });
        }
        self.timeline
            .clip_mut(clip_id)
            .expect("clip existence checked above")
            .audio_role = role;
        Ok(())
    }

    /// Move a clip to `to_track` at `new_start`, preserving duration and source.
    pub fn move_clip(
        &mut self,
        clip_id: ClipId,
        to_track: TrackId,
        new_start: RationalTime,
    ) -> Result<(), ModelError> {
        let tl_rate = self.timeline.frame_rate;
        check_same_rate(new_start.rate, tl_rate)?;
        if new_start.value < 0 {
            return Err(ModelError::InvalidRange);
        }

        let from_track = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let clip_content = &self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?
            .content;

        let dest = self
            .timeline
            .track(to_track)
            .ok_or(ModelError::UnknownTrack(to_track))?;

        if !dest.kind.accepts_content(clip_content) {
            return Err(ModelError::IncompatibleTrackKind {
                track: to_track,
                kind: dest.kind,
            });
        }

        let duration = self
            .timeline
            .clip(clip_id)
            .expect("clip is on a track")
            .timeline
            .duration
            .value;
        let new_tl = TimeRange::at_rate(new_start.value, duration, tl_rate);

        let ignore = (from_track == to_track).then_some(clip_id);
        if dest.has_overlap(new_tl, ignore)? {
            return Err(ModelError::Overlap(to_track));
        }

        if from_track == to_track {
            self.timeline
                .clip_mut(clip_id)
                .expect("clip is on a track")
                .timeline = new_tl;
        } else {
            let mut clip = self
                .timeline
                .remove_clip(clip_id)
                .expect("clip is on a track");
            clip.timeline = new_tl;
            self.timeline.add_clip(to_track, clip)?;
        }
        Ok(())
    }

    /// Shift every clip on `track_id` whose start is at or after `from` by
    /// `delta` ticks (ripple primitive: opens a hole for an insert when
    /// positive, closes a gap when negative).
    ///
    /// Validated atomically: shifting left must not collide with the nearest
    /// unshifted clip or push the first shifted clip below tick 0. Relative
    /// spacing among the shifted clips is preserved, so no other overlap can
    /// arise. Returns the new start of the first shifted clip, or `None` when
    /// no clip starts at/after `from` (a no-op).
    pub fn shift_clips(
        &mut self,
        track_id: TrackId,
        from: RationalTime,
        delta: RationalTime,
    ) -> Result<Option<RationalTime>, ModelError> {
        let tl_rate = self.timeline.frame_rate;
        check_same_rate(from.rate, tl_rate)?;
        check_same_rate(delta.rate, tl_rate)?;
        if delta.value == 0 {
            return Err(ModelError::InvalidRange);
        }

        let track = self
            .timeline
            .track(track_id)
            .ok_or(ModelError::UnknownTrack(track_id))?;

        // Clips never overlap, so the shifted set is a contiguous suffix in
        // start order; only its first member can collide when moving left.
        let mut first_shifted: Option<i64> = None;
        let mut prev_end: i64 = 0;
        for clip in track.clips() {
            let start = clip.timeline.start.value;
            if start >= from.value {
                first_shifted = Some(first_shifted.map_or(start, |s| s.min(start)));
            } else {
                prev_end = prev_end.max(clip.timeline.end_tick());
            }
        }
        let Some(first) = first_shifted else {
            return Ok(None);
        };

        let new_first = first + delta.value;
        if new_first < 0 {
            return Err(ModelError::InvalidRange);
        }
        if delta.value < 0 && new_first < prev_end {
            return Err(ModelError::Overlap(track_id));
        }

        let track = self
            .timeline
            .track_mut(track_id)
            .expect("track existence checked above");
        for clip in track.clips_mut() {
            if clip.timeline.start.value >= from.value {
                clip.timeline.start = time_add(&clip.timeline.start, &delta)?;
            }
        }
        Ok(Some(RationalTime::new(new_first, tl_rate)))
    }

    /// Delete a clip and slide later clips on its track left to close the gap.
    pub fn ripple_delete(&mut self, clip_id: ClipId) -> Result<Clip, ModelError> {
        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let removed = self
            .timeline
            .remove_clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let gap_start = removed.timeline.start;
        let gap = removed.timeline.duration;

        let track = self
            .timeline
            .track_mut(track_id)
            .expect("track existence checked above");
        for clip in track.clips_mut() {
            if clip.timeline.start.value >= gap_start.value {
                clip.timeline.start = time_sub(&clip.timeline.start, &gap)?;
            }
        }
        Ok(removed)
    }
}

fn paths_refer_to_same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// The effect at `index` on a clip's chain, or an out-of-range error.
fn effect_mut(clip: &mut Clip, index: u32) -> Result<&mut EffectInstance, ModelError> {
    clip.effects
        .get_mut(index as usize)
        .ok_or_else(|| ModelError::InvalidParam(format!("effect index {index} out of range")))
}

/// A generated clip's generator, or an error for media-backed clips (shape
/// params route here; the generator itself rejects non-shape kinds).
fn generator_mut(clip: &mut Clip) -> Result<&mut Generator, ModelError> {
    match &mut clip.content {
        ClipSource::Generated(generator) => Ok(generator),
        ClipSource::Media { .. } => Err(ModelError::InvalidParam(
            "shape parameters apply only to generated clips".into(),
        )),
    }
}

/// Reject splits that the current clip representation cannot express without
/// changing the rendered result. Ordinary keyframes can be rebased, while
/// edge-anchored fades/look animations are partitioned after these checks.
fn validate_split_render_continuity(
    clip: &Clip,
    left_duration: i64,
    right_duration: i64,
    rate: Rational,
) -> Result<(), ModelError> {
    if clip.fade_in > left_duration {
        return Err(ModelError::InvalidParam(
            "cannot split inside a clip's fade-in".into(),
        ));
    }
    if clip.fade_out > right_duration {
        return Err(ModelError::InvalidParam(
            "cannot split inside a clip's fade-out".into(),
        ));
    }

    if clip.animation_combo.is_some() {
        let period = look_animation_combo_period_ticks(rate);
        if left_duration % period != 0 {
            return Err(ModelError::InvalidParam(format!(
                "cannot split a combo-animated clip away from its {period}-tick phase boundary"
            )));
        }
    } else {
        let original_window = look_animation_window_ticks(clip.timeline.duration.value, rate);
        if clip.animation_in.is_some()
            && look_animation_window_ticks(left_duration, rate) != original_window
        {
            return Err(ModelError::InvalidParam(
                "split would retime the clip's entrance animation".into(),
            ));
        }
        if clip.animation_out.is_some()
            && look_animation_window_ticks(right_duration, rate) != original_window
        {
            return Err(ModelError::InvalidParam(
                "split would retime the clip's exit animation".into(),
            ));
        }
    }

    match &clip.content {
        ClipSource::Generated(Generator::Lottie { .. }) => Err(ModelError::InvalidParam(
            "cannot split a Lottie clip without an animation-time offset".into(),
        )),
        ClipSource::Generated(Generator::Sticker { asset })
            if crate::sticker::sticker_spec(asset).is_some_and(|spec| spec.animated) =>
        {
            Err(ModelError::InvalidParam(
                "cannot split an animated sticker without an animation-time offset".into(),
            ))
        }
        ClipSource::Media { .. } | ClipSource::Generated(_) => Ok(()),
    }
}

/// Timeline ticks a retimed clip occupies: `source ÷ (base_speed × average
/// ramp)`. A flat ramp keeps the exact integer division M1 used (no f64
/// drift on the common constant-speed path); an active ramp folds in its
/// average multiplier. Always at least one tick.
fn retimed_duration(src_dur_tl: i64, speed: Rational, average: f64, has_curve: bool) -> i64 {
    if !has_curve {
        return (src_dur_tl * i64::from(speed.den) / i64::from(speed.num)).max(1);
    }
    let base = f64::from(speed.num) / f64::from(speed.den);
    let effective = base * average;
    if effective <= 0.0 {
        return src_dur_tl.max(1);
    }
    (src_dur_tl as f64 / effective).round().max(1.0) as i64
}

/// Unwrap a scalar [`ParamValue`] (effect params are always scalar).
fn scalar_param(value: ParamValue) -> Result<f32, ModelError> {
    match value {
        ParamValue::Scalar(v) => Ok(v),
        ParamValue::Vec2(_) | ParamValue::Color(_) => Err(ModelError::InvalidParam(
            "effect parameters take a scalar value".into(),
        )),
    }
}

#[cfg(test)]
mod tests;
