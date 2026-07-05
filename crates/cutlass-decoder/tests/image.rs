//! Still-image probe + decode, end to end on real files.
//!
//! PNG fixtures are written by the `png` crate (also the portable decode
//! path) so tests need no checked-in binary; the JPEG fixture is checked in
//! (`tests/fixtures/halves.jpg`, 32×32, left half red / right half blue)
//! because the portable JPEG path is decode-only.

use std::path::{Path, PathBuf};

use cutlass_decoder::image::{ImageFormat, portable};
use cutlass_decoder::{decode_image, probe, probe_image, sniff_image};

/// Write a `width`×`height` RGBA PNG where the left half is `left` and the
/// right half is `right`.
fn write_png(path: &Path, width: u32, height: u32, left: [u8; 4], right: [u8; 4]) {
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for _y in 0..height {
        for x in 0..width {
            let px = if x < width / 2 { left } else { right };
            pixels.extend_from_slice(&px);
        }
    }
    writer.write_image_data(&pixels).expect("png data");
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn assert_near(actual: [u8; 4], expected: [u8; 4], tolerance: u8, what: &str) {
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert!(
            a.abs_diff(*e) <= tolerance,
            "{what}: channel {i} = {a}, expected ~{e} (±{tolerance}); got {actual:?}"
        );
    }
}

fn pixel(image: &cutlass_core::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * image.width + x) * 4) as usize;
    image.pixels[idx..idx + 4].try_into().unwrap()
}

// --- sniff on real files ---------------------------------------------------

#[test]
fn sniffs_real_png_and_jpeg_files() {
    let dir = tempfile::tempdir().unwrap();
    let png_path = dir.path().join("pic.png");
    write_png(&png_path, 8, 8, [255, 0, 0, 255], [0, 0, 255, 255]);
    assert_eq!(sniff_image(&png_path), Some(ImageFormat::Png));
    assert_eq!(sniff_image(&fixture("halves.jpg")), Some(ImageFormat::Jpeg));
    // Extension lies are irrelevant: sniffing reads bytes, not names.
    let disguised = dir.path().join("actually_a_png.mp4");
    std::fs::copy(&png_path, &disguised).unwrap();
    assert_eq!(sniff_image(&disguised), Some(ImageFormat::Png));
    // Non-images (and unreadable paths) sniff to None.
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "just some text").unwrap();
    assert_eq!(sniff_image(&text), None);
    assert_eq!(sniff_image(&dir.path().join("missing.png")), None);
}

// --- probe + decode through the platform dispatch ---------------------------

#[test]
fn probes_and_decodes_a_png() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("halves.png");
    write_png(&path, 64, 48, [220, 30, 30, 255], [30, 30, 220, 255]);

    let info = probe_image(&path).expect("probe png");
    assert_eq!((info.width, info.height), (64, 48));

    let image = decode_image(&path).expect("decode png");
    assert_eq!((image.width, image.height), (64, 48));
    assert_eq!(image.pixels.len(), 64 * 48 * 4);
    assert_eq!(pixel(&image, 8, 24), [220, 30, 30, 255], "left half");
    assert_eq!(pixel(&image, 56, 24), [30, 30, 220, 255], "right half");
}

#[test]
fn decode_keeps_straight_alpha() {
    // A semi-transparent red: if any backend hands back premultiplied pixels
    // (CoreGraphics' native draw format), red would come back ~½ as strong.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("translucent.png");
    write_png(&path, 16, 16, [200, 60, 20, 128], [200, 60, 20, 128]);

    let image = decode_image(&path).expect("decode translucent png");
    // ±2: the premultiply → un-premultiply roundtrip may quantize.
    assert_near(pixel(&image, 8, 8), [200, 60, 20, 128], 2, "straight alpha");
}

#[test]
fn probes_and_decodes_the_jpeg_fixture() {
    let path = fixture("halves.jpg");
    let info = probe_image(&path).expect("probe jpeg");
    assert_eq!((info.width, info.height), (32, 32));

    let image = decode_image(&path).expect("decode jpeg");
    assert_eq!((image.width, image.height), (32, 32));
    // JPEG is lossy and chroma-subsampled: sample mid-halves, wide tolerance.
    assert_near(pixel(&image, 8, 16), [220, 30, 30, 255], 16, "left half");
    assert_near(pixel(&image, 24, 16), [30, 30, 220, 255], 16, "right half");
}

#[test]
fn probe_image_rejects_non_images() {
    let dir = tempfile::tempdir().unwrap();
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "just some text").unwrap();
    assert!(probe_image(&text).is_err());
    assert!(decode_image(&text).is_err());
}

// --- the media probe routes stills before the video decoder -----------------

#[test]
fn media_probe_classifies_stills() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.png");
    write_png(&path, 40, 30, [1, 2, 3, 255], [4, 5, 6, 255]);

    let probed = probe(&path).expect("probe still");
    assert!(probed.is_image);
    assert_eq!((probed.width, probed.height), (40, 30));
    assert!(!probed.has_audio);
    assert_eq!(probed.frame_count, 0, "stills carry no intrinsic duration");
}

// --- the portable backend directly (Apple hosts route through ImageIO) ------

#[test]
fn portable_png_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.png");
    write_png(&path, 20, 10, [10, 200, 50, 255], [10, 200, 50, 64]);

    let info = portable::probe(&path, ImageFormat::Png).expect("portable probe");
    assert_eq!((info.width, info.height), (20, 10));
    let image = portable::decode(&path, ImageFormat::Png).expect("portable decode");
    assert_eq!(pixel(&image, 2, 5), [10, 200, 50, 255]);
    assert_eq!(pixel(&image, 18, 5), [10, 200, 50, 64], "alpha untouched");
}

#[test]
fn portable_jpeg_decodes_the_fixture() {
    let path = fixture("halves.jpg");
    let info = portable::probe(&path, ImageFormat::Jpeg).expect("portable probe");
    assert_eq!((info.width, info.height), (32, 32));
    let image = portable::decode(&path, ImageFormat::Jpeg).expect("portable decode");
    assert_eq!((image.width, image.height), (32, 32));
    assert_near(pixel(&image, 8, 16), [220, 30, 30, 255], 16, "left half");
    assert_near(pixel(&image, 24, 16), [30, 30, 220, 255], 16, "right half");
}

#[test]
fn portable_refuses_formats_it_cannot_parse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h.heic");
    std::fs::write(&path, b"\0\0\0\x18ftypheic\0\0\0\0moov").unwrap();
    assert!(portable::probe(&path, ImageFormat::Heif).is_err());
    assert!(portable::decode(&path, ImageFormat::Heif).is_err());
}
