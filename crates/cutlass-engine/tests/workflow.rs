//! End-to-end engine workflows: import, edit, undo/redo.

mod common;

use common::{import_asset, remove_media, rt, small_video_asset, temp_engine, tr};
use cutlass_commands::{Command, EditCommand, EditOutcome, ProjectCommand};
use cutlass_engine::ApplyOutcome;
use cutlass_models::TrackKind;

#[test]
fn import_registers_media_and_cache() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (_dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);

    let media = engine.project().media(media_id).expect("media");
    // Import canonicalizes the source path (dedupe across spellings).
    assert_eq!(media.path(), path.canonicalize().expect("asset exists"));
    assert!(media.width > 0);
    assert!(media.height > 0);
    assert!(media.duration.value > 0);
}

#[test]
fn edit_session_via_commands() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (_dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);
    let track = common::add_track(&mut engine, TrackKind::Video, "V1");

    let clip = match engine
        .apply(Command::Edit(EditCommand::AddClip {
            track,
            media: media_id,
            source: tr(0, 48),
            start: rt(0),
        }))
        .expect("add clip")
    {
        ApplyOutcome::Edited(EditOutcome::Created(id)) => id,
        other => panic!("unexpected {other:?}"),
    };

    let tail = match engine
        .apply(Command::Edit(EditCommand::SplitClip { clip, at: rt(24) }))
        .expect("split")
    {
        ApplyOutcome::Edited(EditOutcome::Created(id)) => id,
        other => panic!("unexpected {other:?}"),
    };

    engine
        .apply(Command::Edit(EditCommand::RippleDelete { clip: tail }))
        .expect("ripple delete");

    assert_eq!(engine.project().timeline().clip_count(), 1);
    assert!(engine.project().clip(clip).is_some());
    assert!(engine.project().clip(tail).is_none());
}

#[test]
fn undo_redo_roundtrip_restores_timeline() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (_dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);
    let track = common::add_track(&mut engine, TrackKind::Video, "V1");

    engine
        .apply(Command::Edit(EditCommand::AddClip {
            track,
            media: media_id,
            source: tr(0, 48),
            start: rt(0),
        }))
        .expect("add");

    assert_eq!(engine.project().timeline().clip_count(), 1);
    assert!(engine.can_undo());

    assert!(engine.undo());
    assert_eq!(engine.project().timeline().clip_count(), 0);
    assert!(engine.can_redo());

    assert!(engine.redo());
    assert_eq!(engine.project().timeline().clip_count(), 1);
}

#[test]
fn failed_command_does_not_mutate_project() {
    let (_dir, mut engine) = temp_engine();
    let track = common::add_track(&mut engine, TrackKind::Video, "V1");
    let fake_media = cutlass_models::MediaId::from_raw(999);

    let err = match engine.apply(Command::Edit(EditCommand::AddClip {
        track,
        media: fake_media,
        source: tr(0, 48),
        start: rt(0),
    })) {
        Err(e) => e,
        Ok(_) => panic!("expected unknown media error"),
    };

    assert!(format!("{err}").contains("unknown media"));
    assert_eq!(engine.project().timeline().clip_count(), 0);
    assert!(
        engine.can_undo(),
        "add-track remains undoable after failed add-clip"
    );
}

#[test]
fn import_via_command_from_missing_file_fails_cleanly() {
    let (_dir, mut engine) = temp_engine();
    let err = match engine.apply(Command::Project(ProjectCommand::Import {
        path: "/nonexistent/cutlass-engine.mp4".into(),
    })) {
        Err(e) => e,
        Ok(_) => panic!("expected import error"),
    };
    assert_eq!(engine.project().media_count(), 0);
    assert!(!engine.can_undo());
    // Native probe import resolves the path before opening the decoder, so a
    // missing file fails cleanly at canonicalization with an I/O error — the
    // pool stays empty and nothing lands in history.
    assert!(
        matches!(err, cutlass_engine::EngineError::Io(_)),
        "expected I/O error for missing file, got: {err}"
    );
}

#[test]
fn reimport_same_path_returns_existing_media_without_duplicating_pool() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (_dir, mut engine) = temp_engine();
    let first = import_asset(&mut engine, &path);
    assert_eq!(engine.project().media_count(), 1);
    assert!(engine.can_undo());

    let second = import_asset(&mut engine, &path);
    assert_eq!(first, second);
    assert_eq!(engine.project().media_count(), 1);
    assert!(engine.can_undo(), "re-import must not push undo history");

    assert!(engine.undo());
    assert_eq!(engine.project().media_count(), 0);
    assert!(engine.project().media(first).is_none());
}

#[test]
fn remove_unreferenced_media_is_undoable() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (_dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);
    assert_eq!(engine.project().media_count(), 1);

    remove_media(&mut engine, media_id);
    assert_eq!(engine.project().media_count(), 0);
    assert!(engine.project().media(media_id).is_none());
    assert!(engine.can_undo());

    // Undo restores the source with its original id so existing clips would
    // reattach; redo drops it again.
    assert!(engine.undo());
    assert_eq!(engine.project().media_count(), 1);
    assert!(engine.project().media(media_id).is_some());

    assert!(engine.redo());
    assert_eq!(engine.project().media_count(), 0);
}

#[test]
fn force_delete_media_cascade_undoes_as_one_step() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (_dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);
    let track = common::add_track(&mut engine, TrackKind::Video, "V1");
    let clip = common::add_media_clip(&mut engine, track, media_id, tr(0, 48), rt(0));

    // Mirror cutlass-ui's `remove_media_and_publish` force path: delete the
    // referencing clip and the source as one history group.
    engine.begin_group();
    engine
        .apply(Command::Edit(EditCommand::RemoveClip { clip }))
        .expect("remove clip");
    engine
        .apply(Command::Project(ProjectCommand::RemoveMedia {
            media: media_id,
        }))
        .expect("remove media");
    engine.commit_group();

    assert_eq!(engine.project().media_count(), 0);
    assert_eq!(engine.project().timeline().clip_count(), 0);
    assert!(engine.can_undo());

    // One undo restores the source *and* the clip that referenced it.
    assert!(engine.undo());
    assert_eq!(engine.project().media_count(), 1);
    assert!(engine.project().media(media_id).is_some());
    assert_eq!(engine.project().timeline().clip_count(), 1);
    assert!(engine.project().clip(clip).is_some());

    // And one redo deletes the lot again.
    assert!(engine.redo());
    assert_eq!(engine.project().media_count(), 0);
    assert_eq!(engine.project().timeline().clip_count(), 0);
}

#[test]
fn remove_referenced_media_is_rejected() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (_dir, mut engine) = temp_engine();
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

    let err = match engine.apply(Command::Project(ProjectCommand::RemoveMedia {
        media: media_id,
    })) {
        Err(e) => e,
        Ok(_) => panic!("expected referenced-media rejection"),
    };
    assert!(
        format!("{err}").contains("referenced"),
        "unexpected error: {err}"
    );
    // The rejected command leaves the pool and timeline untouched.
    assert_eq!(engine.project().media_count(), 1);
    assert_eq!(engine.project().timeline().clip_count(), 1);
}
