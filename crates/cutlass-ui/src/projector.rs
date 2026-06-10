//! Slint view-model projection built from engine snapshots or live projects.

use std::collections::HashMap;
use std::rc::Rc;

use cutlass_models::{Project, TrackKind};
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::palette::track_color;
use crate::snapshot::{ClipSnapshot, ProjectSnapshot, TrackSnapshot};
use crate::{
    Clip as SlintClip, Project as SlintProject, Rational as SlintRational,
    RationalTime as SlintRationalTime, Sequence as SlintSequence, TimeRange as SlintTimeRange,
    Track as SlintTrack, TrackKind as SlintTrackKind,
};

struct TrackProjection {
    clips: Rc<VecModel<SlintClip>>,
    clip_row: HashMap<String, usize>,
}

pub struct Projector {
    project: SlintProject,
    tracks_model: Rc<VecModel<SlintTrack>>,
    tracks: HashMap<String, TrackProjection>,
}

impl Projector {
    pub fn from_snapshot(snapshot: &ProjectSnapshot) -> Self {
        let mut tracks_index = HashMap::with_capacity(snapshot.tracks.len());
        let mut slint_tracks = Vec::with_capacity(snapshot.tracks.len());

        for track in &snapshot.tracks {
            let (slint_track, projection) = track_from_snapshot(track);
            tracks_index.insert(track.id.clone(), projection);
            slint_tracks.push(slint_track);
        }

        let tracks_model = Rc::new(VecModel::from(slint_tracks));
        let slint_project = SlintProject {
            id: SharedString::from(snapshot.id.as_str()),
            title: SharedString::from(snapshot.title.as_str()),
            sequence: SlintSequence {
                id: SharedString::from(snapshot.id.as_str()),
                name: SharedString::from(snapshot.title.as_str()),
                fps: SlintRational {
                    num: snapshot.fps_num,
                    den: snapshot.fps_den,
                },
                drop_frame: false,
                tracks: ModelRc::from(tracks_model.clone()),
                width: snapshot.width,
                height: snapshot.height,
            },
        };

        Self {
            project: slint_project,
            tracks_model,
            tracks: tracks_index,
        }
    }

    pub fn from_engine(project: &Project) -> Self {
        Self::from_snapshot(&crate::snapshot::ProjectSnapshot::from_engine(project))
    }

    #[inline]
    pub fn slint_project(&self) -> &SlintProject {
        &self.project
    }

    pub fn move_clip(&self, track_id: &str, clip_id: &str, new_start_value: i32) -> bool {
        let Some(track) = self.tracks.get(track_id) else {
            return false;
        };
        let Some(&row) = track.clip_row.get(clip_id) else {
            return false;
        };
        let Some(mut clip) = track.clips.row_data(row) else {
            return false;
        };
        clip.timeline_start.value = new_start_value;
        track.clips.set_row_data(row, clip);
        true
    }

    pub fn transfer_clip(
        &mut self,
        source_track_id: &str,
        target_track_id: &str,
        clip_id: &str,
        new_start_value: i32,
    ) -> bool {
        let mut clip = {
            let Some(source) = self.tracks.get_mut(source_track_id) else {
                return false;
            };
            let Some(&row) = source.clip_row.get(clip_id) else {
                return false;
            };
            let Some(clip) = source.clips.row_data(row) else {
                return false;
            };
            source.clips.remove(row);
            source.clip_row.remove(clip_id);
            for (_, r) in source.clip_row.iter_mut() {
                if *r > row {
                    *r -= 1;
                }
            }
            clip
        };

        clip.timeline_start.value = new_start_value;

        let Some(target) = self.tracks.get_mut(target_track_id) else {
            return false;
        };
        let row = target.clips.row_count();
        target.clips.push(clip);
        target.clip_row.insert(clip_id.to_owned(), row);
        true
    }

    pub fn rebuild_from_snapshot(&mut self, snapshot: &ProjectSnapshot) {
        *self = Self::from_snapshot(snapshot);
    }
}

fn track_from_snapshot(track: &TrackSnapshot) -> (SlintTrack, TrackProjection) {
    let mut slint_clips = Vec::with_capacity(track.clips.len());
    let mut clip_row = HashMap::with_capacity(track.clips.len());

    for (row, clip) in track.clips.iter().enumerate() {
        slint_clips.push(clip_from_snapshot(clip));
        clip_row.insert(clip.id.clone(), row);
    }

    let clips_model = Rc::new(VecModel::from(slint_clips));
    let slint_track = SlintTrack {
        id: SharedString::from(track.id.as_str()),
        name: SharedString::from(track.name.as_str()),
        kind: track_kind_to_slint(track.kind),
        color: track_color(track.kind, track.kind_index),
        clips: ModelRc::from(clips_model.clone()),
    };

    (
        slint_track,
        TrackProjection {
            clips: clips_model,
            clip_row,
        },
    )
}

fn clip_from_snapshot(clip: &ClipSnapshot) -> SlintClip {
    let rate = |value: i32| SlintRationalTime {
        value,
        rate: SlintRational {
            num: clip.rate_num,
            den: clip.rate_den,
        },
    };
    SlintClip {
        id: SharedString::from(clip.id.as_str()),
        name: SharedString::from(clip.name.as_str()),
        timeline_start: rate(clip.timeline_start),
        source_range: SlintTimeRange {
            start: rate(clip.source_start),
            duration: rate(clip.duration),
        },
    }
}

#[inline]
pub fn track_kind_to_slint(kind: TrackKind) -> SlintTrackKind {
    match kind {
        TrackKind::Video => SlintTrackKind::Video,
        TrackKind::Audio => SlintTrackKind::Audio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{clip_id_to_str, track_id_to_str};
    use cutlass_models::{Clip, Generator, Rational, TimeRange, TrackKind};

    #[test]
    fn from_engine_empty_project() {
        let project = Project::new("test", Rational::FPS_24);
        let projector = Projector::from_engine(&project);
        assert_eq!(projector.slint_project().sequence.tracks.row_count(), 0);
    }

    #[test]
    fn transfer_clip_moves_row_between_tracks() {
        let mut project = Project::new("test", Rational::FPS_24);
        let v1 = project.add_track(TrackKind::Video, "V1");
        let v2 = project.add_track(TrackKind::Video, "V2");
        let clip_id = project
            .timeline_mut()
            .add_clip(
                v1,
                Clip::generated(
                    Generator::SolidColor { rgba: [1, 2, 3, 4] },
                    tr(0, 50),
                ),
            )
            .unwrap();

        let mut projector = Projector::from_engine(&project);
        let id = clip_id_to_str(clip_id);
        assert!(projector.transfer_clip(
            &track_id_to_str(v1),
            &track_id_to_str(v2),
            &id,
            100,
        ));

        let v1_row = projector.slint_project().sequence.tracks.row_data(0).unwrap();
        assert_eq!(v1_row.clips.row_count(), 0);
        let v2_row = projector.slint_project().sequence.tracks.row_data(1).unwrap();
        assert_eq!(v2_row.clips.row_count(), 1);
        assert_eq!(v2_row.clips.row_data(0).unwrap().timeline_start.value, 100);
    }

    #[test]
    fn move_clip_patches_timeline_start() {
        let mut project = Project::new("test", Rational::FPS_24);
        let track = project.add_track(TrackKind::Video, "V1");
        let clip_id = project
            .timeline_mut()
            .add_clip(
                track,
                Clip::generated(
                    Generator::SolidColor { rgba: [1, 2, 3, 4] },
                    tr(0, 50),
                ),
            )
            .unwrap();

        let projector = Projector::from_engine(&project);
        let id = clip_id_to_str(clip_id);
        let track_id = track_id_to_str(track);
        assert!(projector.move_clip(&track_id, &id, 42));

        let clip = projector
            .slint_project()
            .sequence
            .tracks
            .row_data(0)
            .unwrap()
            .clips
            .row_data(0)
            .unwrap();
        assert_eq!(clip.timeline_start.value, 42);
    }

    fn tr(start: i64, duration: i64) -> TimeRange {
        TimeRange::at_rate(start, duration, Rational::FPS_24)
    }
}
