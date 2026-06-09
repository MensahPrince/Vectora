//! Timeline export to MP4.

mod common;

use std::time::Duration;

use common::{import_asset, rt, small_video_asset, temp_engine, tr};
use cutlass_commands::{Command, ProjectCommand};
use cutlass_decoder::{DecodeOptions, Decoder, HwAccel};
use cutlass_engine::ApplyOutcome;
use cutlass_models::{Generator, TrackKind};

#[test]
fn export_generated_timeline_writes_mp4() {
    let (dir, mut engine) = temp_engine();
    let track = common::add_track(&mut engine, TrackKind::Video, "V1");

    engine
        .apply(cutlass_commands::Command::Edit(
            cutlass_commands::EditCommand::AddGenerated {
                track,
                generator: Generator::SolidColor {
                    rgba: [40, 80, 120, 255],
                },
                timeline: tr(0, 24),
            },
        ))
        .expect("add solid");

    let out = dir.path().join("solid_export.mp4");
    let outcome = engine
        .apply(Command::Project(ProjectCommand::Export {
            path: out.clone(),
        }))
        .expect("export");

    let stats = match outcome {
        ApplyOutcome::Exported { stats } => stats,
        other => panic!("expected Exported, got {other:?}"),
    };
    assert_eq!(stats.frames, 24);
    assert_eq!(stats.width, 1920);
    assert_eq!(stats.height, 1080);
    assert!(out.exists());
    assert!(std::fs::metadata(&out).unwrap().len() > 0);

    let mut dec = Decoder::open_with(&out, DecodeOptions::default().hw_accel(HwAccel::None))
        .expect("open export");
    let frame = dec
        .seek_to_frame(Duration::from_millis(200))
        .expect("seek")
        .expect("decoded frame");
    assert!(frame.width > 0);
}

#[test]
fn export_media_clip_writes_seekable_mp4() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);
    let track = common::add_track(&mut engine, TrackKind::Video, "V1");

    engine
        .apply(cutlass_commands::Command::Edit(
            cutlass_commands::EditCommand::AddClip {
                track,
                media: media_id,
                source: tr(0, 24),
                start: rt(0),
            },
        ))
        .expect("add clip");

    let out = dir.path().join("clip_export.mp4");
    let outcome = engine
        .apply(Command::Project(ProjectCommand::Export { path: out.clone() }))
        .expect("export");

    let stats = match outcome {
        ApplyOutcome::Exported { stats } => stats,
        other => panic!("expected Exported, got {other:?}"),
    };
    assert_eq!(stats.frames, 24);
    let media = engine.project().media(media_id).expect("media");
    assert_eq!(stats.width, media.width);
    assert_eq!(stats.height, media.height);
    assert!(out.exists());
}

#[test]
fn export_empty_timeline_errors() {
    let (_dir, mut engine) = temp_engine();
    let err = engine
        .apply(Command::Project(ProjectCommand::Export {
            path: std::env::temp_dir().join("empty_export.mp4"),
        }))
        .unwrap_err();
    assert!(format!("{err}").contains("no content"));
}
