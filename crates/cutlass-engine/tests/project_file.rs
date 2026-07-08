//! Save / open / load project file workflows.

mod common;

use common::{import_asset, rt, small_video_asset, temp_engine, tr};
use cutlass_commands::{Command, EditCommand, ProjectCommand};
use cutlass_engine::{ApplyOutcome, Engine, EngineConfig};
use cutlass_models::{Project, Rational, TrackKind};

fn engine_config() -> EngineConfig {
    EngineConfig { undo_limit: 8 }
}

#[test]
fn dirty_state_tracks_edits_saves_and_history() {
    let (dir, mut engine) = temp_engine();
    assert!(!engine.is_dirty(), "fresh session starts clean");

    let track = common::add_track(&mut engine, TrackKind::Video, "V1");
    assert!(engine.is_dirty(), "edit marks the session dirty");

    let project_file = dir.path().join("dirty.cutlass");
    common::save_project(&mut engine, &project_file);
    assert!(!engine.is_dirty(), "save clears the dirty flag");
    assert!(engine.can_undo(), "save keeps history");

    engine
        .apply(Command::Edit(EditCommand::RemoveTrack { track }))
        .expect("remove track");
    assert!(engine.is_dirty());

    assert!(engine.undo());
    assert!(
        engine.is_dirty(),
        "undo back to saved content stays dirty (revisions only grow)"
    );
    assert!(engine.redo());
    assert!(engine.is_dirty());

    common::open_project(&mut engine, &project_file);
    assert!(!engine.is_dirty(), "open rebaselines as clean");

    common::add_track(&mut engine, TrackKind::Video, "V2");
    assert!(
        engine.is_dirty(),
        "edits after open dirty the session again"
    );
}

#[test]
fn new_session_resets_project_path_history_and_dirty() {
    let (dir, mut engine) = temp_engine();
    common::add_track(&mut engine, TrackKind::Video, "V1");
    let project_file = dir.path().join("before_new.cutlass");
    common::save_project(&mut engine, &project_file);
    common::add_track(&mut engine, TrackKind::Video, "V2");
    assert!(engine.is_dirty());
    assert!(engine.can_undo());

    engine.new_session();
    assert!(!engine.is_dirty(), "a fresh session starts clean");
    assert!(
        engine.project_path().is_none(),
        "the file binding is dropped"
    );
    assert!(
        !engine.can_undo() && !engine.can_redo(),
        "history is cleared"
    );
    assert_eq!(engine.project().timeline().clip_count(), 0);
    assert_eq!(engine.project().media_count(), 0);
    // A fresh session is seeded with the persistent main track (the one
    // lane that exists without clips), and nothing else.
    let tracks: Vec<_> = engine.project().timeline().tracks_ordered().collect();
    assert_eq!(tracks.len(), 1);
    assert!(tracks[0].main && tracks[0].kind == TrackKind::Video);

    // The old session's file is untouched and reopenable.
    common::open_project(&mut engine, &project_file);
    assert_eq!(engine.project().timeline().tracks_ordered().count(), 1);
}

#[test]
fn save_and_open_roundtrip_restores_session() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);
    let track = common::add_track(&mut engine, TrackKind::Video, "V1");
    engine
        .apply(Command::Edit(EditCommand::AddClip {
            track,
            media: media_id,
            source: tr(0, 48),
            start: rt(0),
        }))
        .expect("add clip");
    assert!(engine.can_undo());

    let project_file = dir.path().join("session.cutlass");
    assert!(matches!(
        engine
            .apply(Command::Project(ProjectCommand::Save {
                path: project_file.clone(),
            }))
            .expect("save"),
        ApplyOutcome::Saved
    ));
    assert_eq!(engine.project_path(), Some(&project_file));
    assert!(
        engine.can_undo(),
        "save does not push undo but keeps prior history"
    );

    let clip_id = engine
        .project()
        .timeline()
        .tracks_ordered()
        .flat_map(|t| t.clips())
        .next()
        .expect("clip")
        .id;
    engine
        .apply(Command::Edit(EditCommand::RippleDelete { clip: clip_id }))
        .expect("clear timeline");
    assert_eq!(engine.project().timeline().clip_count(), 0);

    assert!(matches!(
        engine
            .apply(Command::Project(ProjectCommand::Open {
                path: project_file.clone(),
            }))
            .expect("open"),
        ApplyOutcome::Opened
    ));
    assert_eq!(engine.project().timeline().clip_count(), 1);
    assert_eq!(engine.project().media_count(), 1);
    assert!(!engine.can_undo(), "open clears undo history");
}

#[test]
fn open_fails_when_media_missing() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (dir, mut engine) = temp_engine();
    import_asset(&mut engine, &path);

    let project_file = dir.path().join("offline.cutlass");
    engine
        .apply(Command::Project(ProjectCommand::Save {
            path: project_file.clone(),
        }))
        .expect("save");

    let missing = dir.path().join("gone.mp4");
    let mut offline_project = Project::new("offline", Rational::FPS_24);
    offline_project.add_media(cutlass_models::MediaSource::new(
        &missing,
        1920,
        1080,
        Rational::FPS_24,
        100,
        false,
    ));
    let mut engine2 = Engine::with_project(engine_config(), offline_project).expect("engine");
    let offline = dir.path().join("missing_media.cutlass");
    engine2
        .apply(Command::Project(ProjectCommand::Save {
            path: offline.clone(),
        }))
        .expect("save offline");

    let err = engine
        .apply(Command::Project(ProjectCommand::Open { path: offline }))
        .unwrap_err();
    assert!(format!("{err}").contains("not found"));
}

#[test]
fn load_tolerates_missing_media() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("ghost.mp4");
    let mut fixture = Project::new("ghost", Rational::FPS_24);
    fixture.add_media(cutlass_models::MediaSource::new(
        &missing,
        1280,
        720,
        Rational::FPS_24,
        48,
        false,
    ));
    fixture.add_track(TrackKind::Video, "V1");

    let mut engine = Engine::with_project(engine_config(), fixture).expect("engine");

    let project_file = dir.path().join("ghost.cutlass");
    engine
        .apply(Command::Project(ProjectCommand::Save {
            path: project_file.clone(),
        }))
        .expect("save");

    let mut engine2 = Engine::new(engine_config()).expect("engine");
    assert!(matches!(
        engine2
            .apply(Command::Project(ProjectCommand::Load {
                path: project_file,
            }))
            .expect("load"),
        ApplyOutcome::Loaded
    ));
    assert_eq!(engine2.project().media_count(), 1);
    assert_eq!(
        engine2.project().media_iter().next().unwrap().path(),
        missing.as_path()
    );
}
