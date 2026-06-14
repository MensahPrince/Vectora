//! Shared helpers for `cutlass-decoder` integration tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use cutlass_decoder::{DecodeOptions, DecodedFrame, Decoder, HwAccel, KeyframeIndex, PixelFormat};

pub fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

/// Prefer a small 1080p clip for fast integration runs.
pub fn small_video_asset() -> Option<PathBuf> {
    let dir = assets_dir();
    for name in [
        "15531444_1920_1080_24fps.mp4",
        "6137050-hd_1920_1080_24fps.mp4",
        "15604765_1920_1080_24fps.mp4",
    ] {
        let path = dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    any_mp4_in_assets()
}

pub fn any_mp4_in_assets() -> Option<PathBuf> {
    let dir = assets_dir();
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "mp4"))
}

pub fn open_software(path: &Path) -> Decoder {
    Decoder::open_with(path, DecodeOptions::default().hw_accel(HwAccel::None))
        .expect("open decoder (software)")
}

pub fn open_auto(path: &Path) -> Decoder {
    Decoder::open_with(path, DecodeOptions::default()).expect("open decoder (hw auto)")
}

pub fn build_index(path: &Path) -> KeyframeIndex {
    KeyframeIndex::build(path).expect("build keyframe index")
}

pub fn target_ticks(index: &KeyframeIndex, target: Duration) -> i64 {
    index.duration_to_ticks(target)
}

pub fn decode_first_frame(dec: &mut Decoder) -> DecodedFrame {
    dec.next_frame()
        .expect("decode")
        .expect("at least one frame")
}

pub fn assert_frame_shape(frame: &DecodedFrame) {
    assert!(frame.width > 0);
    assert!(frame.height > 0);
    assert!(!frame.planes.is_empty());
    assert!(frame.pts_ticks >= 0);
}

pub fn assert_yuv420p_plane_layout(frame: &DecodedFrame) {
    assert_eq!(frame.format, PixelFormat::Yuv420p);
    assert_eq!(frame.planes.len(), 3);
    let w = frame.width as usize;
    let h = frame.height as usize;
    assert!(w > 0 && h > 0 && h.is_multiple_of(2));

    let y = &frame.planes[0];
    let u = &frame.planes[1];
    let v = &frame.planes[2];

    assert!(y.stride >= w);
    assert!(u.stride >= w / 2);
    assert!(v.stride >= w / 2);
    assert_eq!(y.data.len(), y.stride * h);
    assert_eq!(u.data.len(), u.stride * (h / 2));
    assert_eq!(v.data.len(), v.stride * (h / 2));
}
