//! Regression coverage for the cross-session undo data-loss bug.
//!
//! A clip created in one session, saved, and reopened must survive a full
//! edit / undo / redo cycle in the next session — split halves keep stable
//! ids, and undoing back to the start never destroys the persisted clip.

mod common;

use common::{add_generated, add_track_for_generator, rt, temp_engine, tr};
use cutlass_commands::{Command, EditCommand, EditOutcome};
use cutlass_engine::ApplyOutcome;
use cutlass_models::{ClipId, Generator, MarkerColor};

fn sticker() -> Generator {
    Generator::SolidColor {
        rgba: [200, 40, 90, 255],
    }
}

fn add_marker(engine: &mut cutlass_engine::Engine, at: i64) {
    match engine
        .apply(Command::Edit(EditCommand::AddMarker {
            at: rt(at),
            name: String::new(),
            color: Some(MarkerColor::Teal),
        }))
        .expect("add marker")
    {
        ApplyOutcome::Edited(EditOutcome::CreatedMarker(_)) => {}
        other => panic!("expected CreatedMarker, got {other:?}"),
    }
}

fn split(engine: &mut cutlass_engine::Engine, clip: ClipId, at: i64) -> ClipId {
    match engine
        .apply(Command::Edit(EditCommand::SplitClip { clip, at: rt(at) }))
        .expect("split")
    {
        ApplyOutcome::Edited(EditOutcome::Created(tail)) => tail,
        other => panic!("expected Created tail, got {other:?}"),
    }
}

fn move_clip(engine: &mut cutlass_engine::Engine, clip: ClipId, start: i64) {
    let track = engine
        .project()
        .timeline()
        .track_of(clip)
        .expect("clip on a track");
    engine
        .apply(Command::Edit(EditCommand::MoveClip {
            clip,
            to_track: track,
            start: rt(start),
        }))
        .expect("move clip");
}

/// The reported scenario: sticker added and saved in a prior session, then
/// reopened, edited, and undone all the way back. The clip must remain.
#[test]
fn reload_then_edit_and_full_undo_redo_preserves_the_clip() {
    let (dir, mut engine) = temp_engine();
    let path = dir.path().join("session.cutlass");

    // --- session 1: create a sticker clip and save --------------------------
    let track = add_track_for_generator(&mut engine, &sticker(), "S1");
    let sticker_id = add_generated(&mut engine, track, sticker(), tr(0, 120));
    engine
        .apply(Command::Project(cutlass_commands::ProjectCommand::Save {
            path: path.clone(),
        }))
        .expect("save");

    // --- session 2: reopen the saved document -------------------------------
    let (_dir2, mut engine) = temp_engine();
    engine
        .apply(Command::Project(cutlass_commands::ProjectCommand::Open {
            path: path.clone(),
        }))
        .expect("open");
    assert!(
        engine.project().clip(sticker_id).is_some(),
        "the persisted clip loads under its saved id"
    );
    assert!(
        !engine.can_undo(),
        "a freshly opened document has no history"
    );

    // Play around: markers, then split the persisted clip and shove the tail.
    add_marker(&mut engine, 30);
    add_marker(&mut engine, 90);
    let tail = split(&mut engine, sticker_id, 60);
    assert_ne!(tail, sticker_id, "the split tail gets its own id");
    assert_eq!(
        engine.project().clip(sticker_id).unwrap().timeline,
        tr(0, 60)
    );
    assert_eq!(engine.project().clip(tail).unwrap().timeline, tr(60, 60));
    move_clip(&mut engine, tail, 200);

    // --- undo everything (4 edits) -----------------------------------------
    for _ in 0..4 {
        assert!(engine.undo(), "each edit undoes");
    }
    assert!(!engine.can_undo(), "back to the loaded baseline");
    assert_eq!(
        engine.project().timeline().clip_count(),
        1,
        "the persisted clip is whole again, not lost"
    );
    assert_eq!(
        engine.project().clip(sticker_id).unwrap().timeline,
        tr(0, 120),
        "merge restored the full range"
    );
    assert_eq!(engine.project().timeline().marker_count(), 0);

    // --- redo everything ----------------------------------------------------
    for _ in 0..4 {
        assert!(engine.redo(), "each edit redoes");
    }
    assert!(!engine.can_redo());
    assert_eq!(engine.project().timeline().clip_count(), 2);
    assert_eq!(engine.project().timeline().marker_count(), 2);
    assert_eq!(
        engine.project().clip(sticker_id).unwrap().timeline,
        tr(0, 60)
    );
    assert!(
        engine.project().clip(tail).is_some(),
        "redo restores the tail under its ORIGINAL id (no drift)"
    );
    assert_eq!(engine.project().clip(tail).unwrap().timeline, tr(200, 60));
}

/// The `redo=true stepped=false` storm in the bug report: editing a split
/// tail, then undoing and redoing, must keep the tail id stable so the deeper
/// redo entry that references it still resolves.
#[test]
fn split_tail_survives_repeated_undo_redo_with_a_stable_id() {
    let (_dir, mut engine) = temp_engine();
    let track = add_track_for_generator(&mut engine, &sticker(), "S1");
    let clip = add_generated(&mut engine, track, sticker(), tr(0, 120));

    let tail = split(&mut engine, clip, 60);
    // Edit the tail: an action recorded *after* the split that references it.
    move_clip(&mut engine, tail, 200);

    // Undo the move and the split, then redo both.
    assert!(engine.undo(), "undo move");
    assert!(engine.undo(), "undo split");
    assert_eq!(engine.project().timeline().clip_count(), 1);

    assert!(engine.redo(), "redo split");
    // The tail must come back under the SAME id; before the fix redo minted a
    // fresh id here and the following redo (the move) failed to find it.
    assert!(
        engine.project().clip(tail).is_some(),
        "redone split keeps the tail id"
    );
    assert!(engine.redo(), "redo move resolves the stable tail id");
    assert_eq!(engine.project().clip(tail).unwrap().timeline, tr(200, 60));

    // A second full oscillation still lands on the same id.
    assert!(engine.undo());
    assert!(engine.undo());
    assert!(engine.redo());
    assert!(engine.redo());
    assert_eq!(engine.project().clip(tail).unwrap().timeline, tr(200, 60));
}
