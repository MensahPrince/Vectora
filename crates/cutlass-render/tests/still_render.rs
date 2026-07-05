//! End-to-end still rendering: a PNG on disk, imported as image media,
//! composited on a headless GPU. Skips cleanly when no GPU is available (CI).

use std::path::Path;

use cutlass_core::{Rational, RationalTime};
use cutlass_models::{MediaSource, Project, TrackKind};
use cutlass_render::Renderer;

const FPS_24: Rational = Rational::FPS_24;

fn rt(value: i64) -> RationalTime {
    RationalTime::new(value, FPS_24)
}

/// Write a `width`×`height` RGBA PNG, left half `left`, right half `right`.
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

fn pixel(image: &cutlass_compositor::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * image.width + x) * 4) as usize;
    image.pixels[idx..idx + 4].try_into().unwrap()
}

fn assert_near(actual: [u8; 4], expected: [u8; 4], tolerance: u8, what: &str) {
    for (a, e) in actual.iter().zip(expected.iter()) {
        assert!(
            a.abs_diff(*e) <= tolerance,
            "{what}: got {actual:?}, expected ~{expected:?} (±{tolerance})"
        );
    }
}

#[test]
fn renders_a_still_image_clip() {
    let dir = tempfile::tempdir().unwrap();
    let png_path = dir.path().join("halves.png");
    // Same 16:9 shape as the canvas so the still covers it edge to edge.
    write_png(&png_path, 320, 180, [220, 30, 30, 255], [30, 30, 220, 255]);

    let mut project = Project::new("p", FPS_24);
    let media = project.add_media(MediaSource::image(&png_path, 320, 180));
    let window = project.media(media).unwrap().full_range();
    let track = project.add_track(TrackKind::Video, "V1");
    project.add_clip(track, media, window, rt(0)).unwrap();

    let Ok(mut renderer) = Renderer::new_headless() else {
        eprintln!("skipping: no headless GPU available");
        return;
    };

    // Default canvas (auto with no video media) is 1920×1080; render at a
    // fitted preview size to keep the readback cheap.
    let frame = renderer
        .render_frame_fit(&project, rt(10), 640, 640)
        .expect("render still");
    assert_eq!((frame.width, frame.height), (640, 360));

    // The still is stretched over the whole canvas: sample both halves.
    // Bilinear scaling + premultiply roundtrip allow small drift.
    assert_near(pixel(&frame, 160, 180), [220, 30, 30, 255], 4, "left half");
    assert_near(pixel(&frame, 480, 180), [30, 30, 220, 255], 4, "right half");

    // The second render of the same still must hit the decode-once cache
    // (same pixels, no re-decode observable through the public API).
    let again = renderer
        .render_frame_fit(&project, rt(20), 640, 640)
        .expect("render still again");
    assert_eq!(frame.pixels, again.pixels);
}

#[test]
fn still_alpha_composites_over_the_canvas_background() {
    let dir = tempfile::tempdir().unwrap();
    let png_path = dir.path().join("translucent.png");
    // 50%-alpha white over a black canvas must land mid-gray: wrong alpha
    // handling (double premultiply) would show up darker.
    write_png(
        &png_path,
        320,
        180,
        [255, 255, 255, 128],
        [255, 255, 255, 128],
    );

    let mut project = Project::new("p", FPS_24);
    let media = project.add_media(MediaSource::image(&png_path, 320, 180));
    let window = project.media(media).unwrap().full_range();
    let track = project.add_track(TrackKind::Video, "V1");
    project.add_clip(track, media, window, rt(0)).unwrap();

    let Ok(mut renderer) = Renderer::new_headless() else {
        eprintln!("skipping: no headless GPU available");
        return;
    };
    let frame = renderer
        .render_frame_fit(&project, rt(0), 320, 320)
        .expect("render translucent still");
    assert_near(pixel(&frame, 160, 90), [128, 128, 128, 255], 6, "50% white");
}

#[test]
fn missing_still_file_errors_instead_of_rendering_garbage() {
    let mut project = Project::new("p", FPS_24);
    let media = project.add_media(MediaSource::image("/nonexistent/pic.png", 100, 100));
    let window = project.media(media).unwrap().full_range();
    let track = project.add_track(TrackKind::Video, "V1");
    project.add_clip(track, media, window, rt(0)).unwrap();

    let Ok(mut renderer) = Renderer::new_headless() else {
        eprintln!("skipping: no headless GPU available");
        return;
    };
    assert!(
        renderer
            .render_frame_fit(&project, rt(0), 320, 320)
            .is_err()
    );
}
