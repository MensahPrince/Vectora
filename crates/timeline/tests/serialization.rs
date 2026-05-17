//! JSON serialization and schema-version tests.

mod support;

use decoder::Rational;
use support::{project_three_clips, snapshot};
use timeline::{
    deserialize_project, serialize_project, ClipId, Project, TimelineError, TrackKind,
    CURRENT_SCHEMA_VERSION,
};

#[test]
fn schema_version_constant_matches_project_new() {
    assert_eq!(Project::new().schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn round_trip_preserves_sources_tracks_clips() {
    let (p, track_id, source_id, clip_ids) = project_three_clips();
    let json = serialize_project(&p).unwrap();
    let back = deserialize_project(&json).unwrap();
    assert_eq!(snapshot(&p), snapshot(&back));
    assert_eq!(back.track(track_id).unwrap().clips.len(), 3);
    assert!(back.sources.contains_key(&source_id));
    for id in clip_ids {
        assert!(back.clip(id).is_ok());
    }
}

#[test]
fn deserialized_project_has_empty_history() {
    let (p, _, _, _) = project_three_clips();
    let back = deserialize_project(&serialize_project(&p).unwrap()).unwrap();
    assert_eq!(back.history.undo_depth(), 0);
    assert_eq!(back.history.redo_depth(), 0);
}

#[test]
fn schema_zero_unsupported() {
    let json = r#"{"schema_version":0,"id":"00000000-0000-0000-0000-000000000000","settings":{"frame_rate":{"num":30,"den":1},"width":1920,"height":1080},"sources":{},"tracks":[],"ids":{"next_source":0,"next_track":0,"next_clip":0}}"#;
    let err = deserialize_project(json).unwrap_err();
    assert!(matches!(
        err,
        TimelineError::SchemaUnsupported { found: 0, .. }
    ));
}

#[test]
fn malformed_json_returns_serde_error() {
    let err = deserialize_project("{ not json").unwrap_err();
    assert!(matches!(err, TimelineError::Serde(_)));
}

#[test]
fn rational_times_survive_round_trip() {
    let (p, _, _, _) = project_three_clips();
    let json = serialize_project(&p).unwrap();
    assert!(json.contains("\"num\""));
    let back = deserialize_project(&json).unwrap();
    let clip = back.clip(ClipId(10)).unwrap().1;
    assert_eq!(
        clip.timeline_position,
        Rational::new_raw(0, 1)
    );
}

#[test]
fn default_video_track_round_trip() {
    let p = Project::new().with_default_video_track();
    let back = deserialize_project(&serialize_project(&p).unwrap()).unwrap();
    assert_eq!(back.tracks.len(), 1);
    assert!(matches!(back.tracks[0].kind, TrackKind::Video));
}

#[test]
fn project_id_uuid_preserved() {
    let p = Project::new();
    let id = p.id;
    let back = deserialize_project(&serialize_project(&p).unwrap()).unwrap();
    assert_eq!(back.id, id);
}

#[test]
fn clone_clears_history_but_keeps_edit_state() {
    let (p, _, _, _) = project_three_clips();
    let p2 = p.clone();
    assert_eq!(p2.history.undo_depth(), 0);
    assert_eq!(snapshot(&p), snapshot(&p2));
}
