use std::path::PathBuf;

use decoder::Rational;

use crate::command::Command;
use crate::error::TimelineError;
use crate::ids::{ClipId, MediaSourceId, TrackId};
use crate::model::{find_overlap, validate_clip_fields, Clip, MediaSource, Project, ProbedInfo, TrackKind};
use crate::time;

// --- AddSource ---

pub struct AddSource {
    path: PathBuf,
    allocated_id: Option<MediaSourceId>,
}

impl AddSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            allocated_id: None,
        }
    }
}

impl Command for AddSource {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        let id = project.alloc_source_id();
        self.allocated_id = Some(id);
        project.sources.insert(
            id,
            MediaSource {
                id,
                original_path: self.path.clone(),
                proxy_path: None,
                probed: None,
            },
        );
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(id) = self.allocated_id {
            let clips = project.clips_using_source(id);
            if clips.is_empty() {
                project.sources.remove(&id);
            }
        }
    }

    fn label(&self) -> &str {
        "add source"
    }
}

// --- RemoveSource ---

pub struct RemoveSource {
    source_id: MediaSourceId,
    removed: Option<MediaSource>,
}

impl RemoveSource {
    pub fn new(source_id: MediaSourceId) -> Self {
        Self {
            source_id,
            removed: None,
        }
    }
}

impl Command for RemoveSource {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        let clips = project.clips_using_source(self.source_id);
        if !clips.is_empty() {
            return Err(TimelineError::SourceInUse {
                source_id: self.source_id,
                by_clips: clips,
            });
        }
        self.removed = project.sources.remove(&self.source_id);
        if self.removed.is_none() {
            return Err(TimelineError::SourceNotFound(self.source_id));
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(src) = self.removed.take() {
            project.sources.insert(self.source_id, src);
        }
    }

    fn label(&self) -> &str {
        "remove source"
    }
}

// --- SetSourceProbed (system) ---

pub struct SetSourceProbed {
    source_id: MediaSourceId,
    info: ProbedInfo,
    previous: Option<ProbedInfo>,
}

impl SetSourceProbed {
    pub fn new(source_id: MediaSourceId, info: ProbedInfo) -> Self {
        Self {
            source_id,
            info,
            previous: None,
        }
    }
}

impl Command for SetSourceProbed {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        let src = project
            .sources
            .get_mut(&self.source_id)
            .ok_or(TimelineError::SourceNotFound(self.source_id))?;
        self.previous = src.probed.take();
        src.probed = Some(self.info.clone());
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(src) = project.sources.get_mut(&self.source_id) {
            src.probed = self.previous.take();
        }
    }

    fn label(&self) -> &str {
        "set source probed"
    }
}

// --- AddClip ---

pub struct AddClip {
    track_id: TrackId,
    clip: Clip,
}

impl AddClip {
    pub fn new(track_id: TrackId, clip: Clip) -> Self {
        Self { track_id, clip }
    }
}

impl Command for AddClip {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        project.insert_clip(self.track_id, self.clip.clone())
    }

    fn undo(&mut self, project: &mut Project) {
        let _ = project.remove_clip(self.clip.id);
    }

    fn label(&self) -> &str {
        "add clip"
    }
}

// --- RemoveClip ---

pub struct RemoveClip {
    clip_id: ClipId,
    track_id: Option<TrackId>,
    removed: Option<Clip>,
    index: Option<usize>,
}

impl RemoveClip {
    pub fn new(clip_id: ClipId) -> Self {
        Self {
            clip_id,
            track_id: None,
            removed: None,
            index: None,
        }
    }
}

impl Command for RemoveClip {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        for track in &mut project.tracks {
            if let Some(idx) = track.clips.iter().position(|c| c.id == self.clip_id) {
                self.track_id = Some(track.id);
                self.index = Some(idx);
                self.removed = Some(track.clips.remove(idx));
                return Ok(());
            }
        }
        Err(TimelineError::ClipNotFound(self.clip_id))
    }

    fn undo(&mut self, project: &mut Project) {
        if let (Some(track_id), Some(idx), Some(clip)) =
            (self.track_id, self.index, self.removed.clone())
        {
            if let Ok(track) = project.track_mut(track_id) {
                track.clips.insert(idx.min(track.clips.len()), clip);
            }
        }
    }

    fn label(&self) -> &str {
        "remove clip"
    }
}

// --- MoveClip ---

pub struct MoveClip {
    clip_id: ClipId,
    new_position: Rational,
    old_position: Option<Rational>,
}

impl MoveClip {
    pub fn new(clip_id: ClipId, new_position: Rational) -> Self {
        Self {
            clip_id,
            new_position,
            old_position: None,
        }
    }
}

impl Command for MoveClip {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        let (track_id, clip) = {
            let (_, clip) = project.clip(self.clip_id)?;
            (project
                .tracks
                .iter()
                .find(|t| t.clips.iter().any(|c| c.id == self.clip_id))
                .map(|t| t.id)
                .ok_or(TimelineError::ClipNotFound(self.clip_id))?, clip.clone())
        };
        self.old_position = Some(clip.timeline_position);
        let mut moved = clip;
        moved.timeline_position = self.new_position;
        validate_clip_fields(&moved, project)?;
        let track = project.track_mut(track_id)?;
        if track.locked {
            return Err(TimelineError::InvalidTrim("track is locked"));
        }
        if let Some(existing) = find_overlap(&track.clips, &moved, Some(self.clip_id)) {
            return Err(TimelineError::ClipOverlap {
                existing: existing.id,
                attempted_position: self.new_position.to_string(),
            });
        }
        let idx = track
            .clips
            .iter()
            .position(|c| c.id == self.clip_id)
            .expect("clip on track");
        track.clips.remove(idx);
        let insert_at = track
            .clips
            .partition_point(|c| time::le(c.timeline_position, self.new_position));
        track.clips.insert(insert_at, moved);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(old) = self.old_position {
            let _ = MoveClip {
                clip_id: self.clip_id,
                new_position: old,
                old_position: None,
            }
            .apply(project);
        }
    }

    fn label(&self) -> &str {
        "move clip"
    }
}

// --- TrimClipIn / TrimClipOut ---

pub struct TrimClipIn {
    clip_id: ClipId,
    new_in: Rational,
    old_in: Option<Rational>,
}

impl TrimClipIn {
    pub fn new(clip_id: ClipId, new_in: Rational) -> Self {
        Self {
            clip_id,
            new_in,
            old_in: None,
        }
    }
}

impl Command for TrimClipIn {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        let (track_idx, clip_idx) = locate_clip(project, self.clip_id)?;
        let clip = project.tracks[track_idx].clips[clip_idx].clone();
        self.old_in = Some(clip.source_in);
        if !time::lt(self.new_in, clip.source_out) {
            return Err(TimelineError::InvalidTrim(
                "source_in must be before source_out",
            ));
        }
        let delta = time::sub(self.new_in, clip.source_in).ok_or(TimelineError::InvalidTrim(
            "trim in delta overflow",
        ))?;
        let mut updated = clip;
        updated.source_in = self.new_in;
        updated.timeline_position = time::add(updated.timeline_position, delta)
            .ok_or(TimelineError::InvalidTrim("timeline position overflow"))?;
        validate_clip_fields(&updated, project)?;
        revalidate_track_overlap(project, &updated, Some(self.clip_id))?;
        project.tracks[track_idx].clips[clip_idx] = updated;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(old) = self.old_in {
            let _ = TrimClipIn {
                clip_id: self.clip_id,
                new_in: old,
                old_in: None,
            }
            .apply(project);
        }
    }

    fn label(&self) -> &str {
        "trim clip in"
    }
}

pub struct TrimClipOut {
    clip_id: ClipId,
    new_out: Rational,
    old_out: Option<Rational>,
}

impl TrimClipOut {
    pub fn new(clip_id: ClipId, new_out: Rational) -> Self {
        Self {
            clip_id,
            new_out,
            old_out: None,
        }
    }
}

impl Command for TrimClipOut {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        let (track_idx, clip_idx) = locate_clip(project, self.clip_id)?;
        let clip = project.tracks[track_idx].clips[clip_idx].clone();
        self.old_out = Some(clip.source_out);
        if !time::lt(clip.source_in, self.new_out) {
            return Err(TimelineError::InvalidTrim(
                "source_out must be after source_in",
            ));
        }
        let mut updated = clip;
        updated.source_out = self.new_out;
        validate_clip_fields(&updated, project)?;
        revalidate_track_overlap(project, &updated, Some(self.clip_id))?;
        project.tracks[track_idx].clips[clip_idx] = updated;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(old) = self.old_out {
            let _ = TrimClipOut {
                clip_id: self.clip_id,
                new_out: old,
                old_out: None,
            }
            .apply(project);
        }
    }

    fn label(&self) -> &str {
        "trim clip out"
    }
}

// --- AddTrack / RemoveTrack ---

pub struct AddTrack {
    kind: TrackKind,
    allocated_id: Option<TrackId>,
}

impl AddTrack {
    pub fn new(kind: TrackKind) -> Self {
        Self {
            kind,
            allocated_id: None,
        }
    }
}

impl Command for AddTrack {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        let id = project.add_track(self.kind);
        self.allocated_id = Some(id);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(id) = self.allocated_id {
            if let Ok(track) = project.track(id) {
                if track.clips.is_empty() {
                    project.tracks.retain(|t| t.id != id);
                }
            }
        }
    }

    fn label(&self) -> &str {
        "add track"
    }
}

pub struct RemoveTrack {
    track_id: TrackId,
    removed: Option<crate::model::Track>,
    index: Option<usize>,
}

impl RemoveTrack {
    pub fn new(track_id: TrackId) -> Self {
        Self {
            track_id,
            removed: None,
            index: None,
        }
    }
}

impl Command for RemoveTrack {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
        let idx = project
            .tracks
            .iter()
            .position(|t| t.id == self.track_id)
            .ok_or(TimelineError::TrackNotFound(self.track_id))?;
        if !project.tracks[idx].clips.is_empty() {
            return Err(TimelineError::InvalidTrim("track still has clips"));
        }
        self.index = Some(idx);
        self.removed = Some(project.tracks.remove(idx));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) {
        if let (Some(idx), Some(track)) = (self.index, self.removed.take()) {
            project.tracks.insert(idx.min(project.tracks.len()), track);
        }
    }

    fn label(&self) -> &str {
        "remove track"
    }
}

fn locate_clip(project: &Project, id: ClipId) -> Result<(usize, usize), TimelineError> {
    for (ti, track) in project.tracks.iter().enumerate() {
        if let Some(ci) = track.clips.iter().position(|c| c.id == id) {
            return Ok((ti, ci));
        }
    }
    Err(TimelineError::ClipNotFound(id))
}

fn revalidate_track_overlap(
    project: &Project,
    clip: &Clip,
    ignore: Option<ClipId>,
) -> Result<(), TimelineError> {
    let track = project
        .tracks
        .iter()
        .find(|t| t.clips.iter().any(|c| c.id == clip.id))
        .ok_or(TimelineError::ClipNotFound(clip.id))?;
    if let Some(existing) = find_overlap(&track.clips, clip, ignore) {
        return Err(TimelineError::ClipOverlap {
            existing: existing.id,
            attempted_position: clip.timeline_position.to_string(),
        });
    }
    Ok(())
}
