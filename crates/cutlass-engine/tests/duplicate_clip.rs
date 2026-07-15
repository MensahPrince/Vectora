//! Public command, history, and transition coverage for clip duplication.

mod common;

use common::{rt, tr};
use cutlass_commands::{Command, EditCommand, EditOutcome};
use cutlass_engine::{ApplyOutcome, Engine, EngineConfig};
use cutlass_models::{
    AnimationRef, ClipId, Easing, EffectInstance, Generator, LinkId, Param, Project, TrackId,
    TrackKind,
};

fn engine_with_project(project: Project) -> Engine {
    Engine::with_project(EngineConfig { undo_limit: 32 }, project).expect("engine")
}

fn has_transition(engine: &Engine, track: TrackId, left: ClipId) -> bool {
    engine
        .project()
        .timeline()
        .track(track)
        .unwrap()
        .transition_at(left)
        .is_some()
}

#[test]
fn duplicate_command_undo_redo_restores_exact_id_and_snapshot() {
    let mut project = Project::new("duplicate", cutlass_models::Rational::FPS_24);
    let track = project.add_track(TrackKind::Sticker, "Stickers");
    let source = project
        .add_generated(
            track,
            Generator::SolidColor {
                rgba: [17, 34, 51, 255],
            },
            tr(0, 24),
        )
        .unwrap();
    let companion = project
        .add_generated(
            track,
            Generator::SolidColor {
                rgba: [51, 34, 17, 255],
            },
            tr(24, 24),
        )
        .unwrap();
    let group = LinkId::next();
    for id in [source, companion] {
        project.timeline_mut().clip_mut(id).unwrap().link = Some(group);
    }
    {
        let clip = project.timeline_mut().clip_mut(source).unwrap();
        clip.transform
            .position
            .set_keyframe(0, [-0.25, 0.5], Easing::EaseIn);
        clip.transform
            .position
            .set_keyframe(20, [0.5, -0.25], Easing::EaseOut);
        clip.transform.rotation = Param::Constant(33.0);
        let mut effect = EffectInstance::new("gaussian_blur");
        effect
            .set_param_keyframe(0, 0, 3.0, Easing::EaseInOut)
            .unwrap();
        effect
            .set_param_keyframe(0, 20, 15.0, Easing::EaseOut)
            .unwrap();
        clip.effects.push(effect);
        clip.animation_combo = Some(AnimationRef::new("pulse"));
        clip.replaceable = Some(cutlass_models::Replaceable::new(2).with_label("Copy me"));
        clip.text_editable = true;
    }
    let source_before = project.clip(source).unwrap().clone();

    let mut engine = engine_with_project(project);
    let outcome = engine
        .apply(Command::Edit(EditCommand::DuplicateClip {
            clip: source,
            to_track: track,
            start: rt(100),
        }))
        .expect("duplicate");
    let duplicate = match outcome {
        ApplyOutcome::Edited(EditOutcome::Created(id)) => id,
        other => panic!("expected Created, got {other:?}"),
    };
    let duplicate_before_undo = engine.project().clip(duplicate).unwrap().clone();

    let mut expected = source_before.clone();
    expected.id = duplicate;
    expected.timeline.start = rt(100);
    expected.link = None;
    assert_eq!(duplicate_before_undo, expected);
    assert_eq!(engine.project().clip(source).unwrap(), &source_before);
    assert_eq!(engine.project().clip(companion).unwrap().link, Some(group));

    assert!(engine.undo());
    assert!(engine.project().clip(duplicate).is_none());
    assert_eq!(engine.project().clip(source).unwrap(), &source_before);
    assert_eq!(engine.project().clip(companion).unwrap().link, Some(group));

    assert!(engine.redo());
    assert_eq!(
        engine.project().clip(duplicate).unwrap(),
        &duplicate_before_undo,
        "redo reinserts the captured clip with the same id and full snapshot"
    );
    assert_eq!(engine.project().clip(source).unwrap(), &source_before);
}

#[test]
fn duplicate_preserves_valid_transitions_and_prunes_stale_destination_junctions() {
    let mut project = Project::new("transitions", cutlass_models::Rational::FPS_24);
    let source_track = project.add_track(TrackKind::Sticker, "Source");
    let source = project
        .add_generated(
            source_track,
            Generator::SolidColor {
                rgba: [255, 0, 0, 255],
            },
            tr(0, 24),
        )
        .unwrap();
    let source_right = project
        .add_generated(
            source_track,
            Generator::SolidColor {
                rgba: [0, 0, 255, 255],
            },
            tr(24, 24),
        )
        .unwrap();
    project.add_transition(source, "crossfade").unwrap();

    let destination_track = project.add_track(TrackKind::Sticker, "Destination");
    let stale_left = project
        .add_generated(
            destination_track,
            Generator::SolidColor {
                rgba: [0, 255, 0, 255],
            },
            tr(0, 24),
        )
        .unwrap();
    let stale_right = project
        .add_generated(
            destination_track,
            Generator::SolidColor {
                rgba: [255, 255, 0, 255],
            },
            tr(24, 24),
        )
        .unwrap();
    project.add_transition(stale_left, "wipe_left").unwrap();
    project
        .move_clip(stale_left, destination_track, rt(60))
        .unwrap();

    assert!(
        project
            .timeline()
            .track(source_track)
            .unwrap()
            .transition_at(source)
            .is_some()
    );
    assert!(
        project
            .timeline()
            .track(destination_track)
            .unwrap()
            .transition_at(stale_left)
            .is_some(),
        "the model move leaves pruning to structural dispatch"
    );

    let mut engine = engine_with_project(project);
    let outcome = engine
        .apply(Command::Edit(EditCommand::DuplicateClip {
            clip: source,
            to_track: destination_track,
            start: rt(100),
        }))
        .expect("duplicate");
    let duplicate = match outcome {
        ApplyOutcome::Edited(EditOutcome::Created(id)) => id,
        other => panic!("expected Created, got {other:?}"),
    };

    assert!(
        engine
            .project()
            .timeline()
            .track(destination_track)
            .unwrap()
            .transition_at(duplicate)
            .is_none(),
        "the source junction transition is not copied"
    );
    assert!(
        has_transition(&engine, source_track, source),
        "valid source junction remains"
    );
    assert_eq!(
        engine
            .project()
            .timeline()
            .track(source_track)
            .unwrap()
            .transition_at(source)
            .unwrap()
            .right,
        source_right
    );
    assert!(
        engine
            .project()
            .timeline()
            .track(destination_track)
            .unwrap()
            .transition_at(stale_left)
            .is_none(),
        "structural finalization prunes the stale destination junction"
    );
    assert!(engine.project().clip(stale_right).is_some());

    assert!(engine.undo());
    assert!(engine.project().clip(duplicate).is_none());
    assert!(has_transition(&engine, source_track, source));
    assert!(
        engine
            .project()
            .timeline()
            .track(destination_track)
            .unwrap()
            .transition_at(stale_left)
            .is_some(),
        "undo restores the transition snapshot"
    );

    assert!(engine.redo());
    assert!(engine.project().clip(duplicate).is_some());
    assert!(has_transition(&engine, source_track, source));
    assert!(
        engine
            .project()
            .timeline()
            .track(destination_track)
            .unwrap()
            .transition_at(stale_left)
            .is_none()
    );
}
