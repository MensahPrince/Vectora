//! Main-track magnet primitives (Phase 7): `ShiftClips` / `RippleInsert`
//! through the command surface, and the worker's ripple gesture compositions
//! (reorder dance, move-off-with-gap-close, pack) as single history entries.

mod common;

use common::{
    add_generated, add_media_clip, add_track, import_asset, rt, small_video_asset, temp_engine, tr,
};
use cutlass_commands::{Command, EditCommand, EditOutcome};
use cutlass_engine::ApplyOutcome;
use cutlass_models::{ClipId, Generator, TrackKind};

fn adjustment() -> Generator {
    Generator::Adjustment
}

fn starts(engine: &cutlass_engine::Engine, clips: &[ClipId]) -> Vec<i64> {
    clips
        .iter()
        .map(|c| {
            engine
                .project()
                .clip(*c)
                .expect("clip placed")
                .start()
                .value
        })
        .collect()
}

#[test]
fn shift_clips_command_records_one_invertible_entry() {
    let (_dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Adjustment, "FX");
    let a = add_generated(&mut engine, track, adjustment(), tr(0, 50));
    let b = add_generated(&mut engine, track, adjustment(), tr(50, 30));

    let outcome = engine
        .apply(Command::Edit(EditCommand::ShiftClips {
            track,
            from: rt(50),
            delta: rt(40),
        }))
        .expect("shift");
    assert_eq!(
        outcome,
        ApplyOutcome::Edited(EditOutcome::ShiftedTrack(track))
    );
    assert_eq!(starts(&engine, &[a, b]), [0, 90]);

    assert!(engine.undo());
    assert_eq!(starts(&engine, &[a, b]), [0, 50]);
    assert!(engine.redo());
    assert_eq!(starts(&engine, &[a, b]), [0, 90]);
}

/// The worker's magnet reorder: park past the content end, close the old
/// gap, open the new hole (post-close space), land — one undo for all four.
#[test]
fn reorder_dance_is_one_history_entry() {
    let (_dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Adjustment, "FX");
    // Packed lane: A [0,50) B [50,80) C [80,120).
    let a = add_generated(&mut engine, track, adjustment(), tr(0, 50));
    let b = add_generated(&mut engine, track, adjustment(), tr(50, 30));
    let c = add_generated(&mut engine, track, adjustment(), tr(80, 40));

    // Move A after C: post-close insertion tick = 120 - 50 = 70.
    engine.begin_group();
    engine
        .apply(Command::Edit(EditCommand::MoveClip {
            clip: a,
            to_track: track,
            start: rt(120), // park: lane content end
        }))
        .expect("park");
    engine
        .apply(Command::Edit(EditCommand::ShiftClips {
            track,
            from: rt(0),
            delta: rt(-50),
        }))
        .expect("close old gap");
    engine
        .apply(Command::Edit(EditCommand::ShiftClips {
            track,
            from: rt(70),
            delta: rt(50),
        }))
        .expect("open new hole");
    engine
        .apply(Command::Edit(EditCommand::MoveClip {
            clip: a,
            to_track: track,
            start: rt(70),
        }))
        .expect("land");
    engine.commit_group();

    assert_eq!(starts(&engine, &[b, c, a]), [0, 30, 70], "B C A packed");

    assert!(engine.undo(), "one undo reverts the whole reorder");
    assert_eq!(starts(&engine, &[a, b, c]), [0, 50, 80]);

    assert!(engine.redo(), "one redo replays it");
    assert_eq!(starts(&engine, &[b, c, a]), [0, 30, 70]);
}

/// The worker's move-off-the-main-lane gesture: free placement on the
/// destination plus a gap-close shift on the source, one history entry.
#[test]
fn move_off_with_gap_close_is_one_history_entry() {
    let (_dir, mut engine) = temp_engine();
    let main = add_track(&mut engine, TrackKind::Adjustment, "FX1");
    let overlay = add_track(&mut engine, TrackKind::Adjustment, "FX2");
    let a = add_generated(&mut engine, main, adjustment(), tr(0, 50));
    let b = add_generated(&mut engine, main, adjustment(), tr(50, 30));

    engine.begin_group();
    engine
        .apply(Command::Edit(EditCommand::MoveClip {
            clip: a,
            to_track: overlay,
            start: rt(200),
        }))
        .expect("move off");
    engine
        .apply(Command::Edit(EditCommand::ShiftClips {
            track: main,
            from: rt(0),
            delta: rt(-50),
        }))
        .expect("close gap");
    engine.commit_group();

    assert_eq!(engine.project().timeline().track_of(a), Some(overlay));
    assert_eq!(starts(&engine, &[b]), [0], "gap closed behind the move");

    assert!(engine.undo());
    assert_eq!(engine.project().timeline().track_of(a), Some(main));
    assert_eq!(starts(&engine, &[a, b]), [0, 50]);
}

/// The pack-on-enable gesture: one shift per gap, one history entry total.
#[test]
fn pack_gaps_as_one_history_entry() {
    let (_dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Adjustment, "FX");
    // Leading gap + inner gap: A [10,30) B [60,80).
    let a = add_generated(&mut engine, track, adjustment(), tr(10, 20));
    let b = add_generated(&mut engine, track, adjustment(), tr(60, 20));

    engine.begin_group();
    engine
        .apply(Command::Edit(EditCommand::ShiftClips {
            track,
            from: rt(10),
            delta: rt(-10),
        }))
        .expect("close leading gap");
    engine
        .apply(Command::Edit(EditCommand::ShiftClips {
            track,
            from: rt(50),
            delta: rt(-30),
        }))
        .expect("close inner gap");
    engine.commit_group();

    assert_eq!(starts(&engine, &[a, b]), [0, 20]);

    assert!(engine.undo(), "one undo restores both gaps");
    assert_eq!(starts(&engine, &[a, b]), [10, 60]);
}

/// `RippleInsert` through the wire surface: a single history entry that
/// shifts and places atomically, undone/redone as one.
#[test]
fn ripple_insert_command_is_single_history_entry() {
    let Some(asset) = small_video_asset() else {
        eprintln!("skipping: no mp4 asset available");
        return;
    };
    let (_dir, mut engine) = temp_engine();
    let media = import_asset(&mut engine, &asset);
    let media_rate = engine.project().media(media).expect("media").frame_rate;
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    let src = |start: i64, dur: i64| cutlass_models::TimeRange::at_rate(start, dur, media_rate);

    let first = add_media_clip(&mut engine, track, media, src(0, 24), rt(0));
    let second = add_media_clip(&mut engine, track, media, src(24, 24), rt(24));

    let outcome = engine
        .apply(Command::Edit(EditCommand::RippleInsert {
            track,
            media,
            source: src(0, 12),
            at: rt(24),
        }))
        .expect("ripple insert");
    let ApplyOutcome::Edited(EditOutcome::Created(inserted)) = outcome else {
        panic!("expected Created, got {outcome:?}");
    };
    let shifted = engine.project().clip(second).unwrap().start().value;
    assert!(shifted > 24, "later clip shifted right");
    assert_eq!(engine.project().clip(inserted).unwrap().start().value, 24);
    assert_eq!(engine.project().clip(first).unwrap().start().value, 0);

    assert!(
        engine.undo(),
        "one undo removes the insert and closes the hole"
    );
    assert!(engine.project().clip(inserted).is_none());
    assert_eq!(engine.project().clip(second).unwrap().start().value, 24);

    assert!(engine.redo());
    assert_eq!(engine.project().timeline().clip_count(), 3);
    assert_eq!(
        engine.project().clip(second).unwrap().start().value,
        shifted
    );
}
