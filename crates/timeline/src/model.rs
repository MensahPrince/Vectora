use std::collections::HashMap;
use std::path::PathBuf;

use decoder::{Rational, SourceInfo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::TimelineError;
use crate::ids::{ClipId, IdAllocator, MediaSourceId, ProjectId, TrackId};
use crate::mapping::{active_clip_on_track, ActiveClip};
use crate::time::{add, sub};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Probed container/decoder metadata for a source (filled by the app after engine open).
pub type ProbedInfo = SourceInfo;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub frame_rate: Rational,
    pub width: u32,
    pub height: u32,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            frame_rate: Rational::new_raw(30, 1),
            width: 1920,
            height: 1080,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSource {
    pub id: MediaSourceId,
    pub original_path: PathBuf,
    pub proxy_path: Option<PathBuf>,
    pub probed: Option<ProbedInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    #[serde(other)]
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub muted: bool,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub source_id: MediaSourceId,
    pub source_in: Rational,
    pub source_out: Rational,
    pub timeline_position: Rational,
}

impl Clip {
    /// Duration on the timeline / in the source trim: `source_out - source_in`.
    pub fn duration(&self) -> Option<Rational> {
        sub(self.source_out, self.source_in)
    }

    /// Exclusive end on the timeline: `timeline_position + duration`.
    pub fn timeline_end(&self) -> Option<Rational> {
        let d = self.duration()?;
        add(self.timeline_position, d)
    }

    /// Half-open interval `[timeline_position, timeline_end)`.
    pub fn contains_timeline_time(&self, t: Rational) -> bool {
        let Some(end) = self.timeline_end() else {
            return false;
        };
        t.ge(self.timeline_position) && crate::time::lt(t, end)
    }
}

/// Root edit state: sources, tracks, clips, and undo history.
#[derive(Debug)]
pub struct Project {
    pub schema_version: u32,
    pub id: ProjectId,
    pub settings: ProjectSettings,
    pub sources: HashMap<MediaSourceId, MediaSource>,
    pub tracks: Vec<Track>,
    pub history: crate::history::History,
    ids: IdAllocator,
}

impl Clone for Project {
    fn clone(&self) -> Self {
        Self {
            schema_version: self.schema_version,
            id: self.id,
            settings: self.settings.clone(),
            sources: self.sources.clone(),
            tracks: self.tracks.clone(),
            history: crate::history::History::default(),
            ids: self.ids.clone(),
        }
    }
}

impl Serialize for Project {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ProjectSnapshot::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Project {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let snap: ProjectSnapshot = ProjectSnapshot::deserialize(deserializer)?;
        Ok(snap.into_project())
    }
}

#[derive(Serialize, Deserialize)]
struct ProjectSnapshot {
    schema_version: u32,
    id: ProjectId,
    settings: ProjectSettings,
    sources: HashMap<MediaSourceId, MediaSource>,
    tracks: Vec<Track>,
    ids: IdAllocator,
}

impl ProjectSnapshot {
    fn from(project: &Project) -> Self {
        Self {
            schema_version: project.schema_version,
            id: project.id,
            settings: project.settings.clone(),
            sources: project.sources.clone(),
            tracks: project.tracks.clone(),
            ids: project.ids.clone(),
        }
    }

    fn into_project(self) -> Project {
        Project {
            schema_version: self.schema_version,
            id: self.id,
            settings: self.settings,
            sources: self.sources,
            tracks: self.tracks,
            history: crate::history::History::default(),
            ids: self.ids,
        }
    }
}

impl Project {
    pub fn new() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: ProjectId(Uuid::new_v4()),
            settings: ProjectSettings::default(),
            sources: HashMap::new(),
            tracks: Vec::new(),
            history: crate::history::History::default(),
            ids: IdAllocator::default(),
        }
    }

    pub fn with_default_video_track(mut self) -> Self {
        self.add_track(TrackKind::Video);
        self
    }

    pub fn alloc_source_id(&mut self) -> MediaSourceId {
        self.ids.alloc_source()
    }

    pub fn alloc_track_id(&mut self) -> TrackId {
        self.ids.alloc_track()
    }

    pub fn alloc_clip_id(&mut self) -> ClipId {
        self.ids.alloc_clip()
    }

    pub fn track(&self, id: TrackId) -> Result<&Track, TimelineError> {
        self.tracks
            .iter()
            .find(|t| t.id == id)
            .ok_or(TimelineError::TrackNotFound(id))
    }

    pub fn track_mut(&mut self, id: TrackId) -> Result<&mut Track, TimelineError> {
        self.tracks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(TimelineError::TrackNotFound(id))
    }

    pub fn clip(&self, id: ClipId) -> Result<(&Track, &Clip), TimelineError> {
        for track in &self.tracks {
            if let Some(clip) = track.clips.iter().find(|c| c.id == id) {
                return Ok((track, clip));
            }
        }
        Err(TimelineError::ClipNotFound(id))
    }

    pub fn source(&self, id: MediaSourceId) -> Result<&MediaSource, TimelineError> {
        self.sources
            .get(&id)
            .ok_or(TimelineError::SourceNotFound(id))
    }

    pub fn clips_using_source(&self, source_id: MediaSourceId) -> Vec<ClipId> {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter(|c| c.source_id == source_id)
            .map(|c| c.id)
            .collect()
    }

    pub fn active_clip_on_track(
        &self,
        track_id: TrackId,
        timeline_time: Rational,
    ) -> Result<Option<ActiveClip>, TimelineError> {
        let track = self.track(track_id)?;
        Ok(active_clip_on_track(track, timeline_time))
    }

    pub fn add_track(&mut self, kind: TrackKind) -> TrackId {
        let id = self.alloc_track_id();
        self.tracks.push(Track {
            id,
            kind,
            clips: Vec::new(),
            muted: false,
            locked: false,
        });
        id
    }

    /// Insert `clip` into `track_id`, keeping clips sorted and non-overlapping.
    pub fn insert_clip(&mut self, track_id: TrackId, clip: Clip) -> Result<(), TimelineError> {
        validate_clip_fields(&clip, self)?;
        let track = self.track_mut(track_id)?;
        if track.locked {
            return Err(TimelineError::InvalidTrim("track is locked"));
        }
        if let Some(existing) = find_overlap(&track.clips, &clip, None) {
            return Err(TimelineError::ClipOverlap {
                existing: existing.id,
                attempted_position: clip.timeline_position.to_string(),
            });
        }
        let pos = clip.timeline_position;
        let idx = track
            .clips
            .partition_point(|c| crate::time::le(c.timeline_position, pos));
        track.clips.insert(idx, clip);
        Ok(())
    }

    pub fn remove_clip(&mut self, clip_id: ClipId) -> Result<Clip, TimelineError> {
        for track in &mut self.tracks {
            if let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) {
                return Ok(track.clips.remove(idx));
            }
        }
        Err(TimelineError::ClipNotFound(clip_id))
    }
}

pub(crate) fn validate_clip_fields(clip: &Clip, project: &Project) -> Result<(), TimelineError> {
    if !project.sources.contains_key(&clip.source_id) {
        return Err(TimelineError::SourceNotFound(clip.source_id));
    }
    if !crate::time::lt(clip.source_in, clip.source_out) {
        return Err(TimelineError::InvalidTrim("source_in must be before source_out"));
    }
    let Some(duration) = clip.duration() else {
        return Err(TimelineError::InvalidTrim("clip duration overflow"));
    };
    if !crate::time::gt(duration, Rational::new_raw(0, 1)) {
        return Err(TimelineError::InvalidTrim("clip duration must be positive"));
    }
    if let Some(probed) = &project.sources[&clip.source_id].probed {
        if let Some(max) = probed.duration {
            if crate::time::gt(clip.source_out, max) {
                return Err(TimelineError::InvalidTrim(
                    "source_out exceeds probed duration",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn find_overlap<'a>(
    clips: &'a [Clip],
    candidate: &Clip,
    ignore: Option<ClipId>,
) -> Option<&'a Clip> {
    let Some(c_end) = candidate.timeline_end() else {
        return None;
    };
    for existing in clips {
        if ignore == Some(existing.id) {
            continue;
        }
        let Some(e_end) = existing.timeline_end() else {
            continue;
        };
        // Half-open intervals overlap iff start < other_end && other_start < end.
        if crate::time::lt(candidate.timeline_position, e_end)
            && crate::time::lt(existing.timeline_position, c_end)
        {
            return Some(existing);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_at(pos: i64, dur: i64) -> Clip {
        Clip {
            id: ClipId(0),
            source_id: MediaSourceId(0),
            source_in: Rational::new_raw(0, 1),
            source_out: Rational::new_raw(dur, 1),
            timeline_position: Rational::new_raw(pos, 1),
        }
    }

    #[test]
    fn new_project_has_schema_and_uuid() {
        let p = Project::new();
        assert_eq!(p.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(!p.id.0.is_nil());
    }

    #[test]
    fn find_overlap_detects_partial_overlap() {
        let existing = vec![clip_at(0, 10)];
        let candidate = clip_at(7, 5);
        assert!(find_overlap(&existing, &candidate, None).is_some());
    }

    #[test]
    fn find_overlap_ignores_self() {
        let existing = vec![clip_at(0, 10)];
        let candidate = clip_at(0, 10);
        assert!(find_overlap(&existing, &candidate, Some(ClipId(0))).is_none());
    }

    #[test]
    fn find_overlap_adjacent_no_overlap() {
        let existing = vec![clip_at(0, 5)];
        let candidate = clip_at(5, 3);
        assert!(find_overlap(&existing, &candidate, None).is_none());
    }

    #[test]
    fn default_settings_frame_rate() {
        let s = ProjectSettings::default();
        assert_eq!(s.frame_rate, Rational::new_raw(30, 1));
        assert_eq!(s.width, 1920);
    }
}
