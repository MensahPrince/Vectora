//! End-to-end Lottie rendering: a file-backed `Generator::Lottie` clip
//! composited on a headless GPU — parse, capped-fps frame sampling, LRU
//! reuse, and the missing-file draw-nothing policy. GPU tests skip cleanly
//! when no adapter is available (CI).

use cutlass_core::{Rational, RationalTime};
use cutlass_models::{CanvasAspect, CanvasSettings, Generator, Project, TrackKind};
use cutlass_render::Renderer;

const FPS_24: Rational = Rational::FPS_24;

fn rt(value: i64) -> RationalTime {
    RationalTime::new(value, FPS_24)
}

fn pixel(image: &cutlass_compositor::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * image.width + x) * 4) as usize;
    image.pixels[idx..idx + 4].try_into().unwrap()
}

/// 100×100, 30 fps, 2 s: a centered 80×80 red rectangle whose opacity
/// animates 100 → 0 over the composition, so early and late frames differ.
const FADING_RECT: &str = r#"{
  "v": "5.7.0", "fr": 30, "ip": 0, "op": 60, "w": 100, "h": 100,
  "layers": [{
    "ty": 4, "ip": 0, "op": 60, "st": 0,
    "ks": {"o": {"a": 1, "k": [{"t": 0, "s": [100]}, {"t": 60, "s": [0]}]},
           "p": {"a": 0, "k": [50, 50]},
           "a": {"a": 0, "k": [0, 0, 0]}, "s": {"a": 0, "k": [100, 100, 100]},
           "r": {"a": 0, "k": 0}},
    "shapes": [
      {"ty": "rc", "p": {"a": 0, "k": [0, 0]}, "s": {"a": 0, "k": [80, 80]}, "r": {"a": 0, "k": 0}},
      {"ty": "fl", "c": {"a": 0, "k": [1, 0, 0, 1]}, "o": {"a": 0, "k": 100}}
    ]
  }]
}"#;

/// A 16:9 project with one Lottie clip centered on a black canvas.
fn lottie_project(path: &str) -> Project {
    let mut project = Project::new("lottie", FPS_24);
    project.timeline_mut().set_canvas(CanvasSettings {
        aspect: CanvasAspect::Wide16x9,
        background: [0, 0, 0],
    });
    let track = project.add_track(TrackKind::Sticker, "S1");
    project
        .add_generated(
            track,
            Generator::lottie(path, 100, 100),
            cutlass_models::TimeRange::at_rate(0, 100, FPS_24),
        )
        .unwrap();
    project
}

fn temp_lottie(source: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "cutlass_lottie_test_{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, source).unwrap();
    path
}

#[test]
fn renders_a_lottie_clip_and_advances_frames() {
    let file = temp_lottie(FADING_RECT);
    let project = lottie_project(file.to_str().unwrap());
    let Ok(mut renderer) = Renderer::new_headless() else {
        eprintln!("skipping: no headless GPU available");
        return;
    };

    let first = renderer
        .render_frame_fit(&project, rt(0), 640, 640)
        .expect("render frame 0");
    assert_eq!((first.width, first.height), (640, 360));

    // The 100-reference-px composition sits centered (~33 px at this canvas
    // height); its interior carries the opaque red rect at t=0.
    let center = pixel(&first, 320, 180);
    assert!(
        center[0] > 150,
        "center should carry the red rect, got {center:?}"
    );
    assert_eq!(
        pixel(&first, 10, 10),
        [0, 0, 0, 255],
        "corner is background"
    );

    // 1.5 s in, the rect has faded to 25% opacity — visibly darker.
    let faded = renderer
        .render_frame_fit(&project, rt(36), 640, 640)
        .expect("render faded frame");
    let faded_center = pixel(&faded, 320, 180);
    assert!(
        faded_center[0] < center[0] - 50,
        "opacity animation should darken the composited rect: t0 {center:?} vs t1.5 {faded_center:?}"
    );

    // Same sampled frame (t=0 again) — served from the LRU, identical pixels.
    let again = renderer
        .render_frame_fit(&project, rt(0), 640, 640)
        .expect("render frame 0 again");
    assert_eq!(first.pixels, again.pixels);

    let _ = std::fs::remove_file(&file);
}

#[test]
fn missing_lottie_file_draws_nothing() {
    let project = lottie_project("/nonexistent/animation.json");
    let Ok(mut renderer) = Renderer::new_headless() else {
        eprintln!("skipping: no headless GPU available");
        return;
    };

    let frame = renderer
        .render_frame_fit(&project, rt(0), 640, 640)
        .expect("a missing lottie file must not fail the frame");
    assert_eq!(
        pixel(&frame, 320, 180),
        [0, 0, 0, 255],
        "canvas stays background where the lottie would draw"
    );
}
