//! GPU vs legacy CPU color conversion paths.

mod common;

use common::{add_generated, add_track, rt, temp_engine, tr};
use cutlass_engine::{ColorConvertPath, Engine, EngineConfig};
use cutlass_models::{Generator, TrackKind};

fn legacy_engine() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = EngineConfig {
        cache_dir: dir.path().join("cache"),
        cache_budget_bytes: 64 * 1024 * 1024,
        undo_limit: 32,
        color_convert: ColorConvertPath::LegacyCpu,
    };
    (dir, Engine::new(config).expect("legacy engine"))
}

#[test]
fn gpu_and_legacy_preview_solid_match() {
    let (_dir_gpu, mut gpu_engine) = temp_engine();
    let (_dir_legacy, mut legacy_engine) = legacy_engine();

    for engine in [&mut gpu_engine, &mut legacy_engine] {
        let track = add_track(engine, TrackKind::Sticker, "T1");
        add_generated(
            engine,
            track,
            Generator::SolidColor {
                rgba: [100, 150, 200, 255],
            },
            tr(0, 24),
        );
    }

    let gpu_frame = gpu_engine.get_frame(rt(0)).expect("gpu frame");
    let legacy_frame = legacy_engine.get_frame(rt(0)).expect("legacy frame");
    assert_eq!(gpu_frame.width, legacy_frame.width);
    assert_eq!(gpu_frame.height, legacy_frame.height);
    assert_eq!(gpu_frame.bytes, legacy_frame.bytes);
}
