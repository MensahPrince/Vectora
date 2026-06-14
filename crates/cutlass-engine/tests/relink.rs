//! Missing-media relink (v1 roadmap M0): `ProjectCommand::RelinkMedia`
//! re-points a pool entry at a re-probed file, in place, not undoable.

mod common;

use cutlass_commands::{Command, ProjectCommand};
use cutlass_engine::{ApplyOutcome, EngineError};
use cutlass_models::MediaId;

use common::{
    add_media_clip, add_track, import_asset, load_project, save_project, temp_engine, tr,
};

#[test]
fn relink_recovers_missing_media_after_tolerant_load() {
    let Some(asset) = common::small_video_asset() else {
        return; // no media fixture in this checkout
    };
    let (dir, mut engine) = temp_engine();

    // Import a copy of the asset so we can "move" it out from under the project.
    let original = dir.path().join("footage.mp4");
    std::fs::copy(&asset, &original).expect("copy asset");
    let media = import_asset(&mut engine, &original);
    let imported = engine.project().media(media).expect("entry").clone();

    let track = add_track(&mut engine, cutlass_models::TrackKind::Video, "V1");
    add_media_clip(&mut engine, track, media, tr(0, 24), tr(0, 24).start);

    let project_file = dir.path().join("session.cutlass");
    save_project(&mut engine, &project_file);

    // Simulate the file moving on disk, then reopen tolerantly (Load).
    let moved = dir.path().join("footage-moved.mp4");
    std::fs::rename(&original, &moved).expect("move media");
    load_project(&mut engine, &project_file);

    let entry = engine.project().media(media).expect("entry survives load");
    assert!(
        !entry.path().exists(),
        "loaded entry still points at the dead path"
    );

    let outcome = engine
        .apply(Command::Project(ProjectCommand::RelinkMedia {
            media,
            path: moved.clone(),
        }))
        .expect("relink");
    assert_eq!(outcome, ApplyOutcome::Relinked { media });

    let entry = engine.project().media(media).expect("entry");
    assert_eq!(entry.id, media, "relink keeps the entry's identity");
    assert_eq!(
        entry.path(),
        moved.canonicalize().expect("canonical").as_path()
    );
    assert!(entry.path().exists());
    // Same file content ⇒ the re-probe reproduces the import's metadata.
    assert_eq!(entry.width, imported.width);
    assert_eq!(entry.height, imported.height);
    assert_eq!(entry.duration, imported.duration);

    // The clip still references the relinked entry untouched.
    let clip_media = engine
        .project()
        .timeline()
        .tracks_ordered()
        .flat_map(|t| t.clips())
        .filter_map(|c| c.media())
        .next();
    assert_eq!(clip_media, Some(media));

    // Relink is a repair, not an edit: dirty for saving, nothing to undo.
    assert!(engine.is_dirty(), "repaired path must be saveable");
    assert!(!engine.can_undo(), "relink records no history entry");
}

#[test]
fn relink_unknown_media_errors() {
    let (_dir, mut engine) = temp_engine();
    let err = engine
        .apply(Command::Project(ProjectCommand::RelinkMedia {
            media: MediaId::from_raw(9999),
            path: std::path::PathBuf::from("/nowhere.mp4"),
        }))
        .expect_err("unknown media must fail");
    assert!(matches!(err, EngineError::Model(_)), "got: {err}");
}

#[test]
fn relink_to_missing_file_errors_and_leaves_entry_untouched() {
    let Some(asset) = common::small_video_asset() else {
        return;
    };
    let (dir, mut engine) = temp_engine();
    let media = import_asset(&mut engine, &asset);
    let before = engine.project().media(media).expect("entry").clone();

    let err = engine
        .apply(Command::Project(ProjectCommand::RelinkMedia {
            media,
            path: dir.path().join("does-not-exist.mp4"),
        }))
        .expect_err("dead target path must fail");
    assert!(matches!(err, EngineError::Io(_)), "got: {err}");
    assert_eq!(
        engine.project().media(media),
        Some(&before),
        "entry unchanged on failure"
    );
}
