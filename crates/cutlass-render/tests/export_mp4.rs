//! End-to-end: resolve → GPU render → native encode → decode back. Apple-only,
//! since that's where the native encoder currently exists.

#![cfg(target_vendor = "apple")]

use std::time::{SystemTime, UNIX_EPOCH};

use cutlass_core::VideoDecoder;
use cutlass_decoder::{AvfDecoder, OutputMode};
use cutlass_models::{
    CanvasAspect, CanvasSettings, Generator, Project, Rational, TextStyle, TimeRange, TrackKind,
};
use cutlass_render::{
    ExportSettings, RenderError, Renderer, export_to_file, export_to_file_observed,
};

fn temp_mp4() -> std::path::PathBuf {
    // A timestamp alone collides: macOS SystemTime ticks in microseconds and
    // parallel tests reach here in the same one. The counter disambiguates.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("cutlass_render_export_{nanos}_{seq}.mp4"))
}

/// A quick 3-frame generated timeline (solid + title) on a 16:9 canvas.
fn test_project() -> Project {
    let fps = Rational::FPS_30;
    let span = TimeRange::at_rate(0, 3, fps);

    let mut project = Project::new("export-test", fps);
    project.timeline_mut().set_canvas(CanvasSettings {
        aspect: CanvasAspect::Wide16x9,
        background: [10, 10, 16],
    });

    let bg = project.add_track(TrackKind::Sticker, "BG");
    project
        .add_generated(bg, Generator::SolidColor { rgba: [40, 80, 160, 255] }, span)
        .unwrap();

    let titles = project.add_track(TrackKind::Text, "Titles");
    project
        .add_generated(
            titles,
            Generator::Text {
                content: "Hi".into(),
                style: TextStyle { size: 160.0, ..TextStyle::default() },
            },
            span,
        )
        .unwrap();
    project
}

#[test]
fn exports_a_timeline_to_a_decodable_mp4() {
    let project = test_project();
    let mut renderer = Renderer::new_headless().expect("headless renderer");
    let path = temp_mp4();
    let frames = export_to_file(&mut renderer, &project, &path).expect("export mp4");
    assert_eq!(frames, 3);

    let meta = std::fs::metadata(&path).expect("output exists");
    assert!(meta.len() > 0, "encoded file is empty");

    let mut decoder = AvfDecoder::open(&path, OutputMode::Cpu).expect("reopen output");
    assert_eq!(decoder.info().display_size, (1920, 1080));
    let frame = decoder.next_frame().expect("decode").expect("a frame");
    assert_eq!((frame.width(), frame.height()), (1920, 1080));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn observed_export_scales_output_and_reports_progress() {
    // The desktop/mobile export-job path: explicit settings + observer.
    let project = test_project();
    let mut renderer = Renderer::new_headless().expect("headless renderer");
    let path = temp_mp4();

    let mut calls: Vec<(u64, u64)> = Vec::new();
    let settings = ExportSettings {
        size: (1280, 720),
        frame_rate: Rational::FPS_30,
    };
    let frames = export_to_file_observed(&mut renderer, &project, &path, settings, &mut |d, t| {
        calls.push((d, t));
        true
    })
    .expect("export mp4");
    assert_eq!(frames, 3);

    // The observer announces the total up front and lands on (total, total).
    assert_eq!(calls.first(), Some(&(0, 3)));
    assert_eq!(calls.last(), Some(&(3, 3)));

    let decoder = AvfDecoder::open(&path, OutputMode::Cpu).expect("reopen output");
    assert_eq!(decoder.info().display_size, (1280, 720));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn cancelled_export_returns_cancelled() {
    let project = test_project();
    let mut renderer = Renderer::new_headless().expect("headless renderer");
    let path = temp_mp4();

    let settings = ExportSettings::for_project(&project);
    let result = export_to_file_observed(&mut renderer, &project, &path, settings, &mut |d, _| {
        d < 1 // stop after the first frame is in
    });
    assert!(matches!(result, Err(RenderError::Cancelled)));

    let _ = std::fs::remove_file(&path);
}
