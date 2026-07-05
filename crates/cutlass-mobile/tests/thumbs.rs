//! Thumbnailer FFI: filmstrip frames for video files and stills.

use cutlass_compositor::GpuContext;
use cutlass_mobile::thumbs::{
    cutlass_thumbnailer_close, cutlass_thumbnailer_duration_seconds, cutlass_thumbnailer_open,
    cutlass_thumbnailer_thumb,
};
use cutlass_mobile::{CutlassImage, cutlass_image_free};

fn has_gpu() -> bool {
    GpuContext::new_headless_blocking().is_ok()
}

fn open(path: &std::path::Path) -> *mut cutlass_mobile::thumbs::CutlassThumbnailer {
    let s = path.to_str().expect("utf8 path");
    unsafe { cutlass_thumbnailer_open(s.as_ptr(), s.len()) }
}

fn center_rgb(img: &CutlassImage) -> [u8; 3] {
    // SAFETY: data/len describe the returned RGBA buffer.
    let pixels = unsafe { std::slice::from_raw_parts(img.data, img.len) };
    let idx = (((img.height / 2) * img.width + img.width / 2) * 4) as usize;
    [pixels[idx], pixels[idx + 1], pixels[idx + 2]]
}

/// A 2-second solid mp4 rendered through the export path.
fn temp_mp4() -> std::path::PathBuf {
    use cutlass_models::{Generator, Project, Rational, TimeRange, TrackKind};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("cutlass_thumbs_{nanos}.mp4"));
    let fps = Rational::FPS_30;
    let mut project = Project::new("fixture", fps);
    let track = project.add_track(TrackKind::Sticker, "BG");
    project
        .add_generated(
            track,
            Generator::SolidColor {
                rgba: [40, 80, 160, 255],
            },
            TimeRange::at_rate(0, 60, fps),
        )
        .unwrap();
    let mut renderer = cutlass_render::Renderer::new_headless().expect("headless renderer");
    cutlass_render::export_to_file(&mut renderer, &project, &path).expect("export fixture");
    path
}

/// A 64×48 solid-green PNG.
fn temp_png(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("still.png");
    let file = std::fs::File::create(&path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 64, 48);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    let pixels: Vec<u8> = std::iter::repeat_n([20u8, 200, 60, 255], 64 * 48)
        .flatten()
        .collect();
    writer.write_image_data(&pixels).expect("png data");
    path
}

/// Whether this host has a native encoder/decoder. Only Apple/Windows/Android
/// do; Linux CI has none, so the video filmstrip test (which encodes an mp4
/// fixture, then decodes it) skips there. Still thumbnails decode PNGs in pure
/// Rust, so that test runs everywhere.
fn native_av() -> bool {
    cfg!(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "android"
    ))
}

#[test]
fn video_filmstrip_thumbs_fit_the_requested_box() {
    if !native_av() {
        return; // no native encoder/decoder for the mp4 round-trip (e.g. Linux CI)
    }
    if !has_gpu() {
        eprintln!("skipping: no GPU adapter");
        return;
    }
    let media = temp_mp4();
    let handle = open(&media);
    assert!(!handle.is_null(), "thumbnailer failed to open");

    let duration = unsafe { cutlass_thumbnailer_duration_seconds(handle) };
    assert!((duration - 2.0).abs() < 0.1, "expected ~2s, got {duration}");

    let thumb = unsafe { cutlass_thumbnailer_thumb(handle, 1.0, 80, 46) };
    assert!(!thumb.data.is_null());
    assert!(thumb.width <= 80 && thumb.height <= 46);
    let rgb = center_rgb(&thumb);
    for (got, want) in rgb.iter().zip([40u8, 80, 160]) {
        assert!(
            got.abs_diff(want) <= 3,
            "solid blue frame within H.264 tolerance, got {rgb:?}"
        );
    }
    unsafe { cutlass_image_free(thumb) };

    // Requests past the end clamp to the last frame instead of failing.
    let tail = unsafe { cutlass_thumbnailer_thumb(handle, 99.0, 80, 46) };
    assert!(!tail.data.is_null());
    unsafe { cutlass_image_free(tail) };

    unsafe { cutlass_thumbnailer_close(handle) };
    let _ = std::fs::remove_file(&media);
}

#[test]
fn still_images_thumbnail_at_any_slot() {
    if !has_gpu() {
        eprintln!("skipping: no GPU adapter");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let png = temp_png(dir.path());
    let handle = open(&png);
    assert!(!handle.is_null(), "still thumbnailer failed to open");

    // Stills report the nominal 5s placement window for slot spacing.
    let duration = unsafe { cutlass_thumbnailer_duration_seconds(handle) };
    assert!(
        (duration - 5.0).abs() < 1e-6,
        "still window, got {duration}"
    );

    for seconds in [0.0, 2.5, 60.0] {
        let thumb = unsafe { cutlass_thumbnailer_thumb(handle, seconds, 64, 64) };
        assert!(!thumb.data.is_null(), "thumb at {seconds}s");
        assert_eq!(center_rgb(&thumb), [20, 200, 60], "green at {seconds}s");
        unsafe { cutlass_image_free(thumb) };
    }

    unsafe { cutlass_thumbnailer_close(handle) };
}

#[test]
fn null_and_missing_inputs_are_safe() {
    let missing = std::path::Path::new("/nonexistent/nope.mp4");
    assert!(open(missing).is_null());

    unsafe {
        assert_eq!(cutlass_thumbnailer_duration_seconds(std::ptr::null()), 0.0);
        let img = cutlass_thumbnailer_thumb(std::ptr::null_mut(), 0.0, 64, 64);
        assert!(img.data.is_null());
        cutlass_thumbnailer_close(std::ptr::null_mut());
    }
}
