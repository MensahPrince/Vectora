//! Project domain ↔ Slint view-model conversion.
//!
//! Slint only sees ordered `[Track]` / `[Clip]` models. All map lookups and
//! command mutations stay on the Rust side; call [`project_to_slint`] after
//! each change to refresh the UI.

use std::rc::Rc;

use slint::{ModelRc, SharedString, VecModel};

use crate::{Clip as SlintClip, Project as SlintProject, Rational as SlintRational, RationalTime as SlintRationalTime, Sequence as SlintSequence, TimeRange as SlintTimeRange, Track as SlintTrack};

use super::{Clip, Project, Rational, RationalTime, Sequence, TimeRange, Track};

pub fn project_to_slint(project: &Project) -> SlintProject {
    SlintProject {
        id: SharedString::from(project.id.as_str()),
        title: SharedString::from(project.title.as_str()),
        sequence: sequence_to_slint(&project.sequence),
    }
}

fn sequence_to_slint(sequence: &Sequence) -> SlintSequence {
    let tracks: Vec<SlintTrack> = sequence
        .track_order
        .iter()
        .filter_map(|id| sequence.tracks.get(id))
        .map(track_to_slint)
        .collect();

    SlintSequence {
        id: SharedString::from(sequence.id.as_str()),
        name: SharedString::from(sequence.name.as_str()),
        fps: rational_to_slint(&sequence.fps),
        drop_frame: sequence.drop_frame,
        tracks: ModelRc::new(Rc::new(VecModel::from(tracks))),
        width: sequence.width,
        height: sequence.height,
    }
}

fn track_to_slint(track: &Track) -> SlintTrack {
    let clips: Vec<SlintClip> = track
        .clip_order
        .iter()
        .filter_map(|id| track.clips.get(id))
        .map(clip_to_slint)
        .collect();

    SlintTrack {
        id: SharedString::from(track.id.as_str()),
        name: SharedString::from(track.name.as_str()),
        clips: ModelRc::new(Rc::new(VecModel::from(clips))),
    }
}

fn clip_to_slint(clip: &Clip) -> SlintClip {
    SlintClip {
        id: SharedString::from(clip.id.as_str()),
        name: SharedString::from(clip.name.as_str()),
        timeline_start: rational_time_to_slint(&clip.timeline_start),
        source_range: time_range_to_slint(&clip.source_range),
    }
}

fn rational_to_slint(r: &Rational) -> SlintRational {
    SlintRational {
        num: r.num,
        den: r.den,
    }
}

fn rational_time_to_slint(rt: &RationalTime) -> SlintRationalTime {
    SlintRationalTime {
        value: rt.value,
        rate: rational_to_slint(&rt.rate),
    }
}

fn time_range_to_slint(range: &TimeRange) -> SlintTimeRange {
    SlintTimeRange {
        start: rational_time_to_slint(&range.start),
        duration: rational_time_to_slint(&range.duration),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::sample_project;
    use slint::Model;

    #[test]
    fn projection_preserves_track_and_clip_order() {
        let domain = sample_project();
        let slint = project_to_slint(&domain);

        assert_eq!(slint.sequence.tracks.row_count(), domain.sequence.track_order.len());

        for (i, track_id) in domain.sequence.track_order.iter().enumerate() {
            let track = domain.sequence.tracks.get(track_id).unwrap();
            let slint_track = slint.sequence.tracks.row_data(i).unwrap();
            assert_eq!(slint_track.id, track.id);
            assert_eq!(slint_track.clips.row_count(), track.clip_order.len());

            for (j, clip_id) in track.clip_order.iter().enumerate() {
                let clip = track.clips.get(clip_id).unwrap();
                let slint_clip = slint_track.clips.row_data(j).unwrap();
                assert_eq!(slint_clip.id, clip.id);
                assert_eq!(slint_clip.timeline_start.value, clip.timeline_start.value);
            }
        }
    }
}
