//! Decode + render smoke: opens a fixture through [`engine::Engine`], renders one frame, prints pixel stats.

use std::path::PathBuf;
use std::time::Duration;

use engine::{Engine, EngineEvent, Rational};
use renderer::{Layer, RenderTarget, Renderer, Transform};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../decoder/tests/assets")
        .join(name)
}

fn main() {
    let mut renderer = Renderer::new().expect("renderer");
    let (engine, rx) = Engine::new();
    let (sid, rid_open) = engine.open(asset("testsrc_h264.mp4"));

    loop {
        match rx.recv_timeout(Duration::from_secs(10)).expect("event") {
            EngineEvent::Opened { request_id, .. } => {
                assert_eq!(request_id, rid_open);
                break;
            }
            EngineEvent::Error { error, .. } => panic!("open failed: {error}"),
            other => eprintln!("skipping {other:?}"),
        }
    }

    let rid_seek = engine.seek_exact(sid, Rational::new_raw(2, 1));
    let frame = loop {
        match rx.recv_timeout(Duration::from_secs(10)).expect("event") {
            EngineEvent::Frame {
                frame,
                request_id: Some(r),
                ..
            } if r == rid_seek => break frame,
            EngineEvent::Error { error, .. } => panic!("seek failed: {error}"),
            _ => {}
        }
    };

    let w = frame.width;
    let h = frame.height;
    let target = RenderTarget::new(renderer.device(), w, h);
    renderer
        .render(
            &[Layer {
                frame,
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &target,
        )
        .expect("render");

    let px = renderer
        .read_pixels_rgba8(&target)
        .expect("readback");
    let n = px.len();
    let sum: u64 = px.iter().map(|&b| u64::from(b)).sum();
    let mean = sum as f64 / n as f64;
    let var: f64 = px
        .iter()
        .map(|&b| {
            let d = f64::from(b) - mean;
            d * d
        })
        .sum::<f64>()
        / n as f64;
    println!(
        "end_to_end: {} RGBA bytes, mean={:.2} var={:.2}",
        n, mean, var
    );
    engine.close(sid);
}
