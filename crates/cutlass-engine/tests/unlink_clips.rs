//! Public command dispatch and history coverage for clip unlinking.

mod common;

use common::{add_generated, add_track, temp_engine, tr};
use cutlass_commands::{Command, EditCommand, EditOutcome};
use cutlass_engine::ApplyOutcome;
use cutlass_models::{Generator, TrackKind};

#[test]
fn unlink_command_undo_redo_dissolves_and_restores_full_group() {
    let (_dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Sticker, "Stickers");
    let a = add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [255, 0, 0, 255],
        },
        tr(0, 24),
    );
    let b = add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [0, 0, 255, 255],
        },
        tr(24, 24),
    );
    engine
        .apply(Command::Edit(EditCommand::AddTransition {
            clip: a,
            transition_id: "crossfade".into(),
        }))
        .expect("add transition");

    let linked = engine
        .apply(Command::Edit(EditCommand::LinkClips { clips: vec![a, b] }))
        .expect("link clips");
    assert_eq!(
        linked,
        ApplyOutcome::Edited(EditOutcome::Updated(a)),
        "LinkClips reports its first input"
    );
    let group = engine.project().clip(a).unwrap().link;
    assert!(group.is_some());
    assert_eq!(engine.project().clip(b).unwrap().link, group);

    let unlinked = engine
        .apply(Command::Edit(EditCommand::UnlinkClips { clips: vec![b] }))
        .expect("unlink clips");
    assert_eq!(
        unlinked,
        ApplyOutcome::Edited(EditOutcome::Updated(b)),
        "UnlinkClips reports its first input"
    );
    assert_eq!(engine.project().clip(a).unwrap().link, None);
    assert_eq!(engine.project().clip(b).unwrap().link, None);
    assert_eq!(engine.project().clip(a).unwrap().start().value, 0);
    assert_eq!(engine.project().clip(b).unwrap().start().value, 24);
    assert!(
        engine
            .project()
            .timeline()
            .track(track)
            .unwrap()
            .transition_at(a)
            .is_some(),
        "unlinking is non-structural"
    );

    assert!(engine.undo());
    assert_eq!(engine.project().clip(a).unwrap().link, group);
    assert_eq!(engine.project().clip(b).unwrap().link, group);

    assert!(engine.redo());
    assert_eq!(engine.project().clip(a).unwrap().link, None);
    assert_eq!(engine.project().clip(b).unwrap().link, None);
    assert!(
        engine
            .project()
            .timeline()
            .track(track)
            .unwrap()
            .transition_at(a)
            .is_some()
    );
}
