//! Shared helpers for timeline integration tests.

#![allow(dead_code)]

use std::path::PathBuf;

use decoder::{PixelFormat, Rational, SourceInfo};
use timeline::{
    serialize_project, AddClip, AddSource, Clip, ClipId, MediaSourceId, Project, SetSourceProbed,
    TrackId,
};

/// Snapshot of editable project state (excludes undo history).
pub fn snapshot(project: &Project) -> String {
    serialize_project(project).expect("serialize")
}

pub fn assert_snapshot_unchanged(project: &Project, before: &str) {
    assert_eq!(
        before,
        snapshot(project),
        "project state changed after failed command"
    );
}

pub fn empty_video_project() -> (Project, TrackId) {
    let p = Project::new().with_default_video_track();
    let track_id = p.tracks[0].id;
    (p, track_id)
}

pub fn project_with_source(path: &str) -> (Project, TrackId, MediaSourceId) {
    let (mut p, track_id) = empty_video_project();
    p.apply(Box::new(AddSource::new(path)), true).unwrap();
    let source_id = *p.sources.keys().next().unwrap();
    (p, track_id, source_id)
}

pub fn clip_on_timeline(
    source_id: MediaSourceId,
    id: ClipId,
    timeline_pos: Rational,
    source_in: Rational,
    source_out: Rational,
) -> Clip {
    Clip {
        id,
        source_id,
        source_in,
        source_out,
        timeline_position: timeline_pos,
    }
}

pub fn add_clip(
    project: &mut Project,
    track_id: TrackId,
    clip: Clip,
    record_history: bool,
) -> Result<(), timeline::TimelineError> {
    project.apply(Box::new(AddClip::new(track_id, clip)), record_history)
}

pub fn probed_info(duration_secs: i64) -> SourceInfo {
    SourceInfo {
        width: 1920,
        height: 1080,
        timebase: Rational::new_raw(1, 30_000),
        duration: Some(Rational::new_raw(duration_secs, 1)),
        pixel_format: PixelFormat::Yuv420p,
    }
}

pub fn set_probed(project: &mut Project, source_id: MediaSourceId, duration_secs: i64) {
    project
        .apply(
            Box::new(SetSourceProbed::new(
                source_id,
                probed_info(duration_secs),
            )),
            false,
        )
        .unwrap();
}

/// Three sequential clips: [0,5), [5,8), [8,12) on the timeline (unit seconds).
pub fn project_three_clips() -> (Project, TrackId, MediaSourceId, [ClipId; 3]) {
    let (mut p, track_id, source_id) = project_with_source("/media/three.mp4");
    let ids = [
        ClipId(10),
        ClipId(11),
        ClipId(12),
    ];
    let specs = [
        (Rational::new_raw(0, 1), Rational::new_raw(0, 1), Rational::new_raw(5, 1)),
        (Rational::new_raw(5, 1), Rational::new_raw(0, 1), Rational::new_raw(3, 1)),
        (Rational::new_raw(8, 1), Rational::new_raw(0, 1), Rational::new_raw(4, 1)),
    ];
    for (id, (pos, sin, sout)) in ids.iter().zip(specs) {
        add_clip(
            &mut p,
            track_id,
            clip_on_timeline(source_id, *id, pos, sin, sout),
            true,
        )
        .unwrap();
    }
    (p, track_id, source_id, ids)
}

pub fn lock_track(project: &mut Project, track_id: TrackId) {
    project.track_mut(track_id).unwrap().locked = true;
}

pub fn r(seconds: i64) -> Rational {
    Rational::new_raw(seconds, 1)
}

pub fn r_frac(num: i64, den: u32) -> Rational {
    Rational::new_raw(num, den)
}

pub fn path(s: &str) -> PathBuf {
    PathBuf::from(s)
}
