//! Still-image import: magic-byte classification routes photos to the image
//! probe, and the pool entry follows the model's still convention (5s default
//! placement, millisecond ticks, no audio).
//!
//! Unlike the mp4 import test this runs on every platform — the portable
//! PNG path needs no native codec.

mod common;

use cutlass_models::{MediaKind, STILL_DEFAULT_DURATION_TICKS, STILL_TICK_RATE, TrackKind};

use common::{add_media_clip, add_track, import_asset, rt, temp_engine};

/// Write a tiny solid RGBA PNG with the `png` crate (the same codec the
/// portable decode path uses).
fn write_test_png(path: &std::path::Path, width: u32, height: u32) {
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    let pixels = vec![90u8; (width * height * 4) as usize];
    writer.write_image_data(&pixels).expect("png data");
}

#[test]
fn imports_a_png_as_still_media_and_places_a_clip() {
    let (dir, mut engine) = temp_engine();
    // The extension is deliberately wrong: classification is by content.
    let path = dir.path().join("photo.dat");
    write_test_png(&path, 640, 480);

    let media = import_asset(&mut engine, &path);
    let entry = engine.project().media(media).expect("media registered");
    assert!(entry.is_image);
    assert_eq!(entry.kind(), MediaKind::Image);
    assert_eq!((entry.width, entry.height), (640, 480));
    assert!(!entry.has_audio);
    // Pool duration is the default placement length, not a file property.
    assert_eq!(entry.frame_rate, STILL_TICK_RATE);
    assert_eq!(entry.duration.value, STILL_DEFAULT_DURATION_TICKS);
    let window = entry.full_range();

    // The still backs a normal timeline clip, and (per the model's still
    // rule) may occupy a window longer than the 5s default.
    let track = add_track(&mut engine, TrackKind::Video, "V1");
    let clip = add_media_clip(&mut engine, track, media, window, rt(0));
    assert!(engine.project().clip(clip).is_some());
}
