#[path = "../src/agent_vision.rs"]
mod agent_vision;

use std::path::Path;

use agent_vision::{
    AgentVision, MAX_VISION_EDGE, asset_label, scratch_project, seconds_to_tick, vision_dimensions,
};
use cutlass_models::{MediaSource, Project, Rational, RationalTime, TimeRange, TrackKind};

#[test]
fn dimensions_reject_zero_and_clamp_both_boundaries() {
    assert!(vision_dimensions(0, 100).is_err());
    assert!(vision_dimensions(100, 0).is_err());
    assert_eq!(vision_dimensions(1, 63).unwrap(), (64, 64));
    assert_eq!(vision_dimensions(64, 64).unwrap(), (64, 64));
    assert_eq!(
        vision_dimensions(MAX_VISION_EDGE, MAX_VISION_EDGE).unwrap(),
        (MAX_VISION_EDGE, MAX_VISION_EDGE)
    );
    assert_eq!(
        vision_dimensions(MAX_VISION_EDGE + 1, u32::MAX).unwrap(),
        (MAX_VISION_EDGE, MAX_VISION_EDGE)
    );
}

#[test]
fn time_is_validated_snapped_at_exact_rates_and_clamped_to_tail() {
    let fps24 = Rational::FPS_24;
    assert_eq!(seconds_to_tick(0.0, fps24, 100).unwrap(), 0);
    assert_eq!(seconds_to_tick(25.0 / 24.0, fps24, 100).unwrap(), 25);
    assert_eq!(seconds_to_tick(1.02, fps24, 100).unwrap(), 24);

    let ntsc = Rational::FPS_29_97;
    assert_eq!(seconds_to_tick(1001.0 / 30_000.0, ntsc, 100).unwrap(), 1);
    assert_eq!(seconds_to_tick(1.0, ntsc, 100).unwrap(), 30);
    let snapped = RationalTime::new(30, ntsc);
    assert!((snapped.seconds() - 1.001).abs() < 1e-12);

    assert_eq!(seconds_to_tick(9_999.0, fps24, 48).unwrap(), 47);
    assert!(seconds_to_tick(-0.001, fps24, 48).is_err());
    assert!(seconds_to_tick(f64::NAN, fps24, 48).is_err());
    assert!(seconds_to_tick(f64::INFINITY, fps24, 48).is_err());
}

#[test]
fn scratch_video_has_one_full_source_clip_on_the_main_lane() {
    let source = MediaSource::new(
        "/synthetic/library/take.mov",
        1920,
        1080,
        Rational::FPS_24,
        48,
        true,
    );
    let (project, frame_time) = scratch_project(source.clone(), 17).unwrap();

    assert_single_full_source_project(&project, &source);
    assert_eq!(frame_time, RationalTime::new(17, Rational::FPS_24));
}

#[test]
fn scratch_still_uses_its_full_nominal_range_and_requested_tick() {
    let source = MediaSource::image("/synthetic/library/still.png", 1600, 900);
    let requested_tick = 1_250;
    let (project, frame_time) = scratch_project(source.clone(), requested_tick).unwrap();

    assert_single_full_source_project(&project, &source);
    assert!(source.is_image);
    assert_eq!(
        frame_time,
        RationalTime::new(requested_tick, source.frame_rate)
    );
}

#[test]
fn asset_label_exposes_only_a_safe_file_name_and_actual_time() {
    let path = Path::new("/private/customer-alpha/unreleased/take 01.mov");
    let label = asset_label(path, RationalTime::new(30, Rational::FPS_29_97));

    assert_eq!(label, "take 01.mov at 1.00s");
    assert!(!label.contains("private"));
    assert!(!label.contains("customer-alpha"));
    assert!(!label.contains("unreleased"));

    let controlled = asset_label(
        Path::new("/private/bad\nname.png"),
        RationalTime::new(0, Rational::FPS_24),
    );
    assert_eq!(controlled, "bad\u{fffd}name.png at 0.00s");
}

#[test]
fn zero_duration_visual_source_is_rejected_without_tail_underflow() {
    let source = MediaSource::new(
        "/synthetic/malformed.mov",
        640,
        360,
        Rational::FPS_24,
        0,
        false,
    );

    assert_eq!(seconds_to_tick(100.0, Rational::FPS_24, 0).unwrap(), 0);
    let error = scratch_project(source, 100).unwrap_err();
    assert!(error.contains("no frames"), "unexpected error: {error}");
}

#[test]
fn audio_only_source_is_rejected_as_nonvisual() {
    let source = MediaSource::new(
        "/synthetic/voice.wav",
        0,
        0,
        Rational::new(1000, 1),
        5_000,
        true,
    );

    let error = scratch_project(source, 0).unwrap_err();
    assert!(error.contains("audio-only"), "unexpected error: {error}");
}

#[test]
fn constructing_the_service_does_not_require_a_gpu() {
    let _new = AgentVision::new();
    let _default = AgentVision::default();
    let _project_frame = AgentVision::project_frame;
    let _asset_frame = AgentVision::asset_frame;
}

fn assert_single_full_source_project(project: &Project, source: &MediaSource) {
    assert_eq!(project.media_count(), 1);
    assert_eq!(project.media(source.id), Some(source));
    assert_eq!(project.timeline().track_count(), 1);
    assert_eq!(project.timeline().clip_count(), 1);

    let main = project.timeline().main_track().expect("main video lane");
    let track = project.timeline().track(main).expect("main track exists");
    assert_eq!(track.kind, TrackKind::Video);
    assert!(track.main);
    assert_eq!(track.len(), 1);

    let clip = track.clips().next().expect("full-source clip");
    assert_eq!(clip.media(), Some(source.id));
    assert_eq!(clip.source_range(), Some(source.full_range()));
    assert_eq!(
        clip.timeline,
        TimeRange::at_rate(0, source.duration.value, source.frame_rate)
    );
}
