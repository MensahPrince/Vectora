use crate::Map;
use crate::error::ModelError;
use crate::ids::{ClipId, MediaId, ProjectId, TrackId};
use crate::media::MediaSource;
use crate::time::{Rational, TimeRange, convert_frames};
use crate::timeline::Timeline;
use crate::track::{Track, TrackKind};
use crate::clip::{Clip, ClipSource, Generator};

/// Top-level container: a media pool plus exactly one [`Timeline`].
///
/// `Project` is the aggregate root and the only place that can guarantee
/// referential integrity between clips and media, so clip creation goes through
/// [`add_clip`](Project::add_clip).
#[derive(Debug, Clone)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    media: Map<MediaId, MediaSource>,
    timeline: Timeline,
}

impl Project {
    /// Create an empty project whose timeline runs at `frame_rate`.
    pub fn new(name: impl Into<String>, frame_rate: Rational) -> Self {
        Self {
            id: ProjectId::next(),
            name: name.into(),
            media: Map::default(),
            timeline: Timeline::new(frame_rate),
        }
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

    pub fn media_iter(&self) -> impl Iterator<Item = &MediaSource> {
        self.media.values()
    }

    pub fn media_count(&self) -> usize {
        self.media.len()
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

    /// Place a clip referencing `media_id` on `track_id`.
    ///
    /// The clip's timeline duration is conformed from the source's frame rate to
    /// the timeline's frame rate, so a 30fps source on a 24fps timeline occupies
    /// the right number of timeline frames. Validates media/track existence,
    /// that `source` is within the media bounds, and that the placement does not
    /// overlap an existing clip on the track.
    pub fn add_clip(
        &mut self,
        track_id: TrackId,
        media_id: MediaId,
        source: TimeRange,
        timeline_start: i64,
    ) -> Result<ClipId, ModelError> {
        let media = self
            .media
            .get(&media_id)
            .ok_or(ModelError::UnknownMedia(media_id))?;

        if source.is_empty() {
            return Err(ModelError::InvalidRange);
        }
        if source.start < 0 || source.end() > media.duration {
            return Err(ModelError::SourceOutOfBounds);
        }

        let timeline_duration =
            convert_frames(source.duration, media.frame_rate, self.timeline.frame_rate);
        let timeline = TimeRange::new(timeline_start, timeline_duration.max(1));

        let clip = Clip::from_media(media_id, source, timeline);
        self.timeline.add_clip(track_id, clip)
    }

    /// Place a generated clip (text, shape, solid, ...) on `track_id`.
    ///
    /// Generated content has no source media, so the caller specifies the
    /// timeline placement directly (in timeline frames).
    pub fn add_generated(
        &mut self,
        track_id: TrackId,
        generator: Generator,
        timeline: TimeRange,
    ) -> Result<ClipId, ModelError> {
        if timeline.is_empty() {
            return Err(ModelError::InvalidRange);
        }
        let clip = Clip::generated(generator, timeline);
        self.timeline.add_clip(track_id, clip)
    }

    /// Find a clip by ID anywhere on the timeline (O(1)).
    pub fn clip(&self, clip_id: ClipId) -> Option<&Clip> {
        self.timeline.clip(clip_id)
    }

    // --- editing primitives ----------------------------------------------
    //
    // These are the invariant-preserving mutations the engine's command layer
    // (and through it, the AI agent and UI) dispatch. Each leaves the timeline
    // in a legal state — no overlaps, no out-of-bounds source ranges — or
    // returns a [`ModelError`] without mutating anything.

    /// Remove a clip from wherever it lives. Returns the removed clip, or
    /// `None` if no clip has that ID. Leaves a gap (use
    /// [`ripple_delete`](Project::ripple_delete) to close it).
    pub fn remove_clip(&mut self, clip_id: ClipId) -> Option<Clip> {
        self.timeline.remove_clip(clip_id)
    }

    /// Split the clip under `clip_id` at timeline frame `at` into two abutting
    /// clips. The original becomes the left half `[start, at)`; a new clip takes
    /// the right half `[at, end)` and is returned.
    ///
    /// `at` must be strictly inside the clip (`start < at < end`). For media
    /// clips the source range is divided at the corresponding source frame
    /// (rate-converted the same way [`resolve`](crate) maps positions), so each
    /// half plays its own portion of the footage.
    pub fn split_clip(&mut self, clip_id: ClipId, at: i64) -> Result<ClipId, ModelError> {
        let clip = self
            .timeline
            .clip(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let tl = clip.timeline;
        if at <= tl.start || at >= tl.end() {
            return Err(ModelError::InvalidRange);
        }
        let left_tl = TimeRange::new(tl.start, at - tl.start);
        let right_tl = TimeRange::new(at, tl.end() - at);
        let tl_fps = self.timeline.frame_rate;

        // Build the right-hand clip and figure out the left clip's new source
        // range from an immutable borrow, then apply mutations afterwards.
        let (new_left_source, new_clip) = match clip.content.clone() {
            ClipSource::Media { media, source } => {
                let media_fps = self
                    .media
                    .get(&media)
                    .ok_or(ModelError::UnknownMedia(media))?
                    .frame_rate;
                if source.duration < 2 {
                    // No room to give each half at least one source frame.
                    return Err(ModelError::InvalidRange);
                }
                let left_src_dur = convert_frames(left_tl.duration, tl_fps, media_fps)
                    .clamp(1, source.duration - 1);
                let left_source = TimeRange::new(source.start, left_src_dur);
                let right_source =
                    TimeRange::new(source.start + left_src_dur, source.duration - left_src_dur);
                (Some(left_source), Clip::from_media(media, right_source, right_tl))
            }
            ClipSource::Generated(generator) => (None, Clip::generated(generator, right_tl)),
        };

        let track_id = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;

        // Shrink the original to the left half first, then add the right half;
        // the freed `[at, end)` no longer overlaps anything.
        {
            let left = self
                .timeline
                .clip_mut(clip_id)
                .expect("clip existence checked above");
            left.timeline = left_tl;
            if let (Some(src), ClipSource::Media { source, .. }) =
                (new_left_source, &mut left.content)
            {
                *source = src;
            }
        }
        self.timeline.add_clip(track_id, new_clip)
    }

    /// Set the clip's timeline placement to `new_timeline` (a trim/extend).
    ///
    /// For media clips the source in-point and length are adjusted to match, so
    /// trimming the head reveals a different start frame and trimming the tail
    /// shortens the played range. Fails if the new range would overlap a
    /// neighbour on the track or fall outside the media's source bounds.
    pub fn trim_clip(&mut self, clip_id: ClipId, new_timeline: TimeRange) -> Result<(), ModelError> {
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
        let tl_fps = self.timeline.frame_rate;

        if self
            .timeline
            .track(track_id)
            .expect("clip is on a track")
            .has_overlap(new_timeline, Some(clip_id))
        {
            return Err(ModelError::Overlap(track_id));
        }

        let new_source = match clip.content.clone() {
            ClipSource::Media { media, source } => {
                let media = self.media.get(&media).ok_or(ModelError::UnknownMedia(media))?;
                let head_delta = convert_frames(new_timeline.start - old_tl.start, tl_fps, media.frame_rate);
                let new_src_start = source.start + head_delta;
                let new_src_dur =
                    convert_frames(new_timeline.duration, tl_fps, media.frame_rate).max(1);
                if new_src_start < 0 || new_src_start + new_src_dur > media.duration {
                    return Err(ModelError::SourceOutOfBounds);
                }
                Some(TimeRange::new(new_src_start, new_src_dur))
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

    /// Move a clip to `to_track`, starting at timeline frame `new_start`,
    /// preserving its duration and source range. Fails on a negative start, an
    /// unknown track, or an overlap at the destination.
    pub fn move_clip(
        &mut self,
        clip_id: ClipId,
        to_track: TrackId,
        new_start: i64,
    ) -> Result<(), ModelError> {
        if new_start < 0 {
            return Err(ModelError::InvalidRange);
        }
        let from_track = self
            .timeline
            .track_of(clip_id)
            .ok_or(ModelError::UnknownClip(clip_id))?;
        let dest = self
            .timeline
            .track(to_track)
            .ok_or(ModelError::UnknownTrack(to_track))?;

        let duration = self
            .timeline
            .clip(clip_id)
            .expect("clip is on a track")
            .timeline
            .duration;
        let new_tl = TimeRange::new(new_start, duration);

        // Ignore the clip itself only when it stays on the same track.
        let ignore = (from_track == to_track).then_some(clip_id);
        if dest.has_overlap(new_tl, ignore) {
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

    /// Delete a clip and close the gap: every clip starting at or after the
    /// deleted clip on the same track shifts earlier by its duration. Returns
    /// the removed clip.
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
            if clip.timeline.start >= gap_start {
                clip.timeline.start -= gap;
            }
        }
        Ok(removed)
    }
}
