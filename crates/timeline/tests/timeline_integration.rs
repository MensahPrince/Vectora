//! Integration tests for timeline commands, mapping, and serialization.

use std::path::PathBuf;

use decoder::{PixelFormat, Rational, SourceInfo};
use timeline::{
    deserialize_project, serialize_project, AddClip, AddSource, AddTrack, Clip, ClipId,
    MediaSourceId, MoveClip, Project, RemoveClip, RemoveSource, RemoveTrack, SetSourceProbed,
    TimelineError, TrackId, TrackKind, TrimClipIn, TrimClipOut,
};

fn sample_project_with_clip() -> (Project, TrackId, MediaSourceId, ClipId) {
    let mut p = Project::new().with_default_video_track();
    let track_id = p.tracks[0].id;
    p.apply(Box::new(AddSource::new("/media/a.mp4")), true)
        .unwrap();
    let source_id = *p.sources.keys().next().unwrap();
    let clip = Clip {
        id: p.alloc_clip_id(),
        source_id,
        source_in: Rational::new_raw(0, 1),
        source_out: Rational::new_raw(10, 1),
        timeline_position: Rational::new_raw(0, 1),
    };
    let clip_id = clip.id;
    p.apply(Box::new(AddClip::new(track_id, clip)), true).unwrap();
    (p, track_id, source_id, clip_id)
}

#[test]
fn active_clip_maps_media_time() {
    let (p, track_id, source_id, _) = sample_project_with_clip();
    let active = p
        .active_clip_on_track(track_id, Rational::new_raw(3, 1))
        .unwrap()
        .expect("clip");
    assert_eq!(active.source_id, source_id);
    assert_eq!(active.media_time.reduced(), Rational::new_raw(3, 1));
}

#[test]
fn overlap_add_clip_rejected() {
    let (mut p, track_id, source_id, _) = sample_project_with_clip();
    let before = serialize_project(&p).unwrap();
    let clip = Clip {
        id: ClipId(999),
        source_id,
        source_in: Rational::new_raw(0, 1),
        source_out: Rational::new_raw(2, 1),
        timeline_position: Rational::new_raw(5, 1),
    };
    let err = p
        .apply(Box::new(AddClip::new(track_id, clip)), true)
        .unwrap_err();
    assert!(matches!(err, TimelineError::ClipOverlap { .. }));
    assert_eq!(before, serialize_project(&p).unwrap());
}

#[test]
fn remove_source_in_use_errors() {
    let (p, _, source_id, _) = sample_project_with_clip();
    let mut p = p;
    let err = p
        .apply(Box::new(RemoveSource::new(source_id)), true)
        .unwrap_err();
    assert!(matches!(
        err,
        TimelineError::SourceInUse { source_id: sid, .. } if sid == source_id
    ));
}

#[test]
fn command_undo_restores_serialized_state() {
    let (mut p, _track_id, _, clip_id) = sample_project_with_clip();
    let before = serialize_project(&p).unwrap();
    p.apply(
        Box::new(MoveClip::new(clip_id, Rational::new_raw(20, 1))),
        true,
    )
    .unwrap();
    assert_ne!(before, serialize_project(&p).unwrap());
    p.undo().unwrap();
    assert_eq!(before, serialize_project(&p).unwrap());
}

#[test]
fn trim_clip_in_undo_round_trip() {
    let (mut p, _, _, clip_id) = sample_project_with_clip();
    let before = serialize_project(&p).unwrap();
    p.apply(
        Box::new(TrimClipIn::new(clip_id, Rational::new_raw(2, 1))),
        true,
    )
    .unwrap();
    p.undo().unwrap();
    assert_eq!(before, serialize_project(&p).unwrap());
}

#[test]
fn trim_clip_out_undo_round_trip() {
    let (mut p, _, _, clip_id) = sample_project_with_clip();
    let before = serialize_project(&p).unwrap();
    p.apply(
        Box::new(TrimClipOut::new(clip_id, Rational::new_raw(8, 1))),
        true,
    )
    .unwrap();
    p.undo().unwrap();
    assert_eq!(before, serialize_project(&p).unwrap());
}

#[test]
fn set_source_probed_skips_history() {
    let (mut p, _, source_id, _) = sample_project_with_clip();
    let depth_before = p.history.undo_depth();
    let info = SourceInfo {
        width: 1280,
        height: 720,
        timebase: Rational::new_raw(1, 30_000),
        duration: Some(Rational::new_raw(100, 1)),
        pixel_format: PixelFormat::Yuv420p,
    };
    p.apply(Box::new(SetSourceProbed::new(source_id, info.clone())), false)
        .unwrap();
    assert_eq!(p.history.undo_depth(), depth_before);
    assert_eq!(p.source(source_id).unwrap().probed.as_ref(), Some(&info));
}

#[test]
fn schema_unsupported_future_version() {
    let json = r#"{"schema_version":99,"id":{"0":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]},"settings":{"frame_rate":{"num":30,"den":1},"width":1920,"height":1080},"sources":{},"tracks":[],"ids":{"next_source":1,"next_track":1,"next_clip":1}}"#;
    let err = deserialize_project(json).unwrap_err();
    assert!(matches!(
        err,
        TimelineError::SchemaUnsupported {
            found: 99,
            supported_max: 1
        }
    ));
}

#[test]
fn add_remove_track_round_trip() {
    let mut p = Project::new();
    p.apply(Box::new(AddTrack::new(TrackKind::Video)), true)
        .unwrap();
    let track_id = p.tracks.last().unwrap().id;
    p.apply(Box::new(RemoveTrack::new(track_id)), true).unwrap();
    p.undo().unwrap();
    p.undo().unwrap();
    assert!(p.tracks.is_empty());
    assert!(p.sources.is_empty());
}

#[test]
fn remove_clip_and_undo() {
    let (mut p, _, _, clip_id) = sample_project_with_clip();
    let before = serialize_project(&p).unwrap();
    p.apply(Box::new(RemoveClip::new(clip_id)), true).unwrap();
    assert!(p.clip(clip_id).is_err());
    p.undo().unwrap();
    assert_eq!(before, serialize_project(&p).unwrap());
}

#[test]
fn add_source_path_round_trip() {
    let mut p = Project::new();
    let path = PathBuf::from("/tmp/clip.mov");
    p.apply(Box::new(AddSource::new(path.clone())), true)
        .unwrap();
    let sid = *p.sources.keys().next().unwrap();
    assert_eq!(p.source(sid).unwrap().original_path, path);
    p.undo().unwrap();
    assert!(p.sources.is_empty());
}
