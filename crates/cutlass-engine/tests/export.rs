//! Timeline export to MP4.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    add_generated, add_media_clip, add_track, export_to, import_asset, rt, small_video_asset,
    temp_engine, tr,
};
use cutlass_commands::{Command, EditCommand, ProjectCommand};
use cutlass_decoder::{DecodeOptions, Decoder, HwAccel};
use cutlass_models::{Generator, TrackKind};

fn open_export(path: &Path) -> Decoder {
    Decoder::open_with(path, DecodeOptions::default().hw_accel(HwAccel::None))
        .unwrap_or_else(|e| panic!("open export {}: {e}", path.display()))
}

fn assert_export_duration_near(path: &Path, frames: u64, fps: i32) {
    let dec = open_export(path);
    let dur = dec.duration().expect("exported file should report duration");
    let expected_ms = (frames as f64 / f64::from(fps)) * 1000.0;
    let actual_ms = dur.as_millis() as f64;
    assert!(
        (actual_ms - expected_ms).abs() < 250.0,
        "duration {actual_ms}ms not near {expected_ms}ms for {frames} frames @ {fps}fps"
    );
}

// --- empty / invalid inputs -----------------------------------------------

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

#[test]
fn export_track_without_clips_errors() {
    let (_dir, mut engine) = temp_engine();
    let _track = add_track(&mut engine, TrackKind::Video, "V1");

    let err = engine
        .apply(Command::Project(ProjectCommand::Export {
            path: std::env::temp_dir().join("track_only_export.mp4"),
        }))
        .unwrap_err();
    assert!(format!("{err}").contains("no content"));
}

#[test]
fn export_missing_output_parent_errors() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [1, 2, 3, 255],
        },
        tr(0, 12),
    );

    let out = dir.path().join("nested/missing/export.mp4");
    let err = engine
        .apply(Command::Project(ProjectCommand::Export { path: out }))
        .unwrap_err();
    assert!(
        format!("{err}").contains("failed to open"),
        "export should fail when parent directory is missing: {err}"
    );
}

// --- solid / generated ----------------------------------------------------

#[test]
fn export_generated_timeline_writes_mp4() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [40, 80, 120, 255],
        },
        tr(0, 24),
    );

    let out = dir.path().join("solid_export.mp4");
    let stats = export_to(&mut engine, &out);

    assert_eq!(stats.frames, 24);
    assert_eq!(stats.width, 1920);
    assert_eq!(stats.height, 1080);
    assert!(out.exists());
    assert!(std::fs::metadata(&out).unwrap().len() > 0);

    let mut dec = open_export(&out);
    let frame = dec
        .seek_to_frame(Duration::from_millis(200))
        .expect("seek")
        .expect("decoded frame");
    assert_eq!(frame.width, 1920);
    assert_eq!(frame.height, 1080);
    assert_export_duration_near(&out, 24, 24);
}

#[test]
fn export_longer_solid_timeline_scales_frame_count() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [255, 0, 0, 255],
        },
        tr(0, 48),
    );

    let out = dir.path().join("solid_48f.mp4");
    let stats = export_to(&mut engine, &out);
    assert_eq!(stats.frames, 48);
    assert_export_duration_near(&out, 48, 24);
}

// --- media ----------------------------------------------------------------

#[test]
fn export_media_clip_writes_seekable_mp4() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_media_clip(&mut engine, track, media_id, tr(0, 24), rt(0));

    let out = dir.path().join("clip_export.mp4");
    let stats = export_to(&mut engine, &out);

    assert_eq!(stats.frames, 24);
    let media = engine.project().media(media_id).expect("media");
    assert_eq!(stats.width, media.width);
    assert_eq!(stats.height, media.height);
    assert!(out.exists());

    let mut dec = open_export(&out);
    let frame = dec
        .seek_to_frame(Duration::ZERO)
        .expect("seek head")
        .expect("frame");
    assert_eq!(frame.width, media.width);
    assert_eq!(frame.height, media.height);
    assert_export_duration_near(&out, 24, 24);
}

// --- edits affecting duration / layout ------------------------------------

#[test]
fn export_trimmed_clip_uses_shortened_duration() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    let clip = add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [5, 10, 15, 255],
        },
        tr(0, 48),
    );

    engine
        .apply(Command::Edit(EditCommand::TrimClip {
            clip,
            timeline: tr(0, 12),
        }))
        .expect("trim");

    let out = dir.path().join("trimmed_export.mp4");
    let stats = export_to(&mut engine, &out);
    assert_eq!(stats.frames, 12);
    assert_export_duration_near(&out, 12, 24);
}

#[test]
fn export_abutting_clips_span_full_timeline() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [255, 0, 0, 255],
        },
        tr(0, 24),
    );
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [0, 255, 0, 255],
        },
        tr(24, 24),
    );

    let out = dir.path().join("abutting_export.mp4");
    let stats = export_to(&mut engine, &out);
    assert_eq!(stats.frames, 48);
    assert_export_duration_near(&out, 48, 24);
}

#[test]
fn export_two_track_composite_writes_valid_mp4() {
    let Some(path) = small_video_asset() else {
        return;
    };
    let (dir, mut engine) = temp_engine();
    let media_id = import_asset(&mut engine, &path);
    let v1 = add_track(&mut engine, TrackKind::Video, "V1");
    let v2 = add_track(&mut engine, TrackKind::Video, "V2");

    add_media_clip(&mut engine, v1, media_id, tr(0, 24), rt(0));
    add_generated(
        &mut engine,
        v2,
        Generator::SolidColor {
            rgba: [0, 0, 0, 128],
        },
        tr(0, 24),
    );

    let out = dir.path().join("stack_export.mp4");
    let stats = export_to(&mut engine, &out);
    let media = engine.project().media(media_id).expect("media");
    assert_eq!(stats.frames, 24);
    assert_eq!(stats.width, media.width);
    assert_eq!(stats.height, media.height);
    assert!(std::fs::metadata(&out).unwrap().len() > 0);
}

#[test]
fn export_duration_follows_latest_ending_track() {
    let (dir, mut engine) = temp_engine();
    let v1 = add_track(&mut engine, TrackKind::Video, "V1");
    let v2 = add_track(&mut engine, TrackKind::Video, "V2");

    add_generated(
        &mut engine,
        v1,
        Generator::SolidColor {
            rgba: [1, 1, 1, 255],
        },
        tr(0, 24),
    );
    add_generated(
        &mut engine,
        v2,
        Generator::SolidColor {
            rgba: [2, 2, 2, 255],
        },
        tr(0, 36),
    );

    let out = dir.path().join("max_end_export.mp4");
    let stats = export_to(&mut engine, &out);
    assert_eq!(stats.frames, 36);
}

// --- known limitations (gaps) ---------------------------------------------

#[test]
fn export_timeline_gap_errors() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [1, 2, 3, 255],
        },
        tr(0, 24),
    );
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [4, 5, 6, 255],
        },
        tr(48, 24),
    );

    let err = engine
        .apply(Command::Project(ProjectCommand::Export {
            path: dir.path().join("gap_export.mp4"),
        }))
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no video") || msg.contains("Preview"),
        "expected gap failure, got: {msg}"
    );
}

#[test]
fn export_delayed_clip_start_errors_on_leading_gap() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [9, 9, 9, 255],
        },
        tr(12, 12),
    );

    let err = engine
        .apply(Command::Project(ProjectCommand::Export {
            path: dir.path().join("delayed_start_export.mp4"),
        }))
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no video") || msg.contains("Preview"),
        "expected leading-gap failure, got: {msg}"
    );
}

// --- engine semantics -----------------------------------------------------

#[test]
fn export_does_not_push_undo_entry() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [1, 2, 3, 255],
        },
        tr(0, 12),
    );
    assert!(engine.can_undo());

    let out = dir.path().join("undo_check.mp4");
    export_to(&mut engine, &out);
    assert!(engine.can_undo(), "export should not clear undo stack");
    assert!(!engine.can_redo());

    assert!(engine.undo());
    assert_eq!(engine.project().timeline().clip_count(), 0);
}

#[test]
fn export_twice_to_same_path_succeeds() {
    let (dir, mut engine) = temp_engine();
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    add_generated(
        &mut engine,
        track,
        Generator::SolidColor {
            rgba: [10, 20, 30, 255],
        },
        tr(0, 12),
    );

    let out = dir.path().join("overwrite.mp4");
    let first = export_to(&mut engine, &out);
    let second = export_to(&mut engine, &out);
    assert_eq!(first, second);
    assert!(out.exists());
}
