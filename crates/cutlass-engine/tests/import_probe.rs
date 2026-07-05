//! End-to-end native import: synthesize a real mp4 with the platform encoder,
//! then probe + import it through the engine.
//!
//! This is the only test that exercises the FFmpeg-free probe against actual
//! decoded metadata, so it needs no checked-in fixture — it writes its own clip
//! with the native encoder first. Apple-only, matching the encoder/decoder
//! backends available in this checkout.

#![cfg(target_vendor = "apple")]

mod common;

use cutlass_core::{
    ColorSpace, CpuImage, EncoderConfig, FrameData, PixelFormat, Plane, Rational, RationalTime,
    Rect, Rotation, VideoEncoder, VideoFrame,
};
use cutlass_encoder::AvfEncoder;
use cutlass_models::TrackKind;

use common::{add_media_clip, add_track, import_asset, rt, temp_engine, tr};

fn solid_rgba_frame(width: u32, height: u32, index: i64, rate: Rational) -> VideoFrame {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..(width * height) {
        pixels.extend_from_slice(&[20, 120, 220, 255]); // opaque blue, RGBA
    }
    let plane = Plane::new(pixels, width as usize * 4, height as usize);
    VideoFrame::new(
        RationalTime::new(index, rate),
        PixelFormat::Rgba8,
        ColorSpace::BT709,
        (width, height),
        Rect::from_size(width, height),
        Rotation::None,
        FrameData::Cpu(CpuImage::new(vec![plane])),
    )
}

/// Write a solid-color `frames`-long mp4 with the native encoder.
fn write_test_mp4(path: &std::path::Path, width: u32, height: u32, rate: Rational, frames: i64) {
    let config = EncoderConfig::new((width, height), rate, ColorSpace::BT709);
    let mut encoder = AvfEncoder::open(path, config).expect("open encoder");
    for i in 0..frames {
        encoder
            .push(&solid_rgba_frame(width, height, i, rate))
            .expect("push frame");
    }
    encoder.finish().expect("finish");
}

#[test]
fn imports_a_synthesized_mp4_and_places_a_clip() {
    let (width, height, frames) = (320u32, 240u32, 12i64);
    // FPS_24 matches the default timeline rate and the tr()/rt() helpers.
    let rate = Rational::FPS_24;
    let (dir, mut engine) = temp_engine();
    let path = dir.path().join("synth.mp4");
    write_test_mp4(&path, width, height, rate, frames);

    // Native, FFmpeg-free probe + media-pool registration.
    let media = import_asset(&mut engine, &path);
    let entry = engine.project().media(media).expect("media registered");
    assert_eq!(
        entry.path(),
        path.canonicalize().expect("canonical").as_path()
    );
    assert_eq!((entry.width, entry.height), (width, height));
    assert_eq!(entry.frame_rate, rate);
    // Frame count is derived from the container duration; allow ±1 for how the
    // muxer accounts the trailing frame's presentation interval.
    assert!(
        (entry.duration.value - frames).abs() <= 1,
        "probed {} frames, expected ~{frames}",
        entry.duration.value
    );
    assert!(!entry.has_audio, "synth clip is video-only");
    assert!(!entry.is_image);

    // The imported source backs a real timeline edit (AddClip-from-path).
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    let clip = add_media_clip(&mut engine, track, media, tr(0, 6), rt(0));
    assert!(engine.project().clip(clip).is_some());
}
