#[path = "../src/timeline_map.rs"]
mod timeline_map;

use cutlass_models::{
    Generator, Marker, MarkerColor, MediaSource, Project, Rational, RationalTime, TimeRange,
    TrackKind,
};
use cutlass_render::RgbaImage;
use timeline_map::{Canvas, TimelineMapOptions, render};

const FPS: Rational = Rational::FPS_24;
const VIDEO: [u8; 4] = [0x4A, 0x6F, 0xA5, 0xFF];
const AUDIO: [u8; 4] = [0xC9, 0x98, 0x46, 0xFF];
const TEXT: [u8; 4] = [0x5E, 0x8B, 0x7E, 0xFF];
const TEAL_MARKER: [u8; 4] = [0x00, 0xE5, 0xC7, 0xFF];
const PLAYHEAD: [u8; 4] = [255, 59, 79, 255];
const OMISSION_BACKGROUND: [u8; 4] = [55, 43, 65, 255];

fn time(ticks: i64) -> RationalTime {
    RationalTime::new(ticks, FPS)
}

fn range(start: i64, duration: i64) -> TimeRange {
    TimeRange::at_rate(start, duration, FPS)
}

fn has_pixel(image: &RgbaImage, color: [u8; 4]) -> bool {
    image.pixels.chunks_exact(4).any(|pixel| pixel == color)
}

fn has_plot_pixel(image: &RgbaImage, color: [u8; 4]) -> bool {
    let x_start = 142u32.min(image.width);
    let x_end = image.width.saturating_sub(8);
    let y_start = 38u32.min(image.height);
    let y_end = image.height.saturating_sub(24);
    (y_start..y_end).any(|y| {
        (x_start..x_end).any(|x| {
            let offset = ((y as usize * image.width as usize) + x as usize) * 4;
            image.pixels[offset..offset + 4] == color
        })
    })
}

#[test]
fn schematic_canvas_centers_rgba_images_for_contact_sheets() {
    let mut canvas = Canvas::new(32, 24, [0, 0, 0, 255]).unwrap();
    let source = RgbaImage::new(4, 2, vec![255; 4 * 2 * 4]);

    assert!(canvas.draw_image_centered(&source, 4, 4, 20, 12));
    let image = canvas.into_image();

    assert_eq!(image.pixel(12, 9), [255; 4]);
    assert_eq!(image.pixel(4, 4), [0, 0, 0, 255]);
}

fn assert_bounded_opaque_image(image: &RgbaImage) {
    assert!((320..=1024).contains(&image.width));
    assert!(image.height <= 768);
    assert!(image.is_well_formed());
    assert!(image.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
}

#[test]
fn empty_project_is_bounded_opaque_and_rejects_invalid_ranges() {
    let project = Project::new("empty", FPS);
    let image = render(
        &project,
        TimelineMapOptions {
            width: 1,
            ..TimelineMapOptions::default()
        },
    )
    .unwrap();
    assert_eq!(image.width, 320);
    assert_bounded_opaque_image(&image);

    let wide = render(
        &project,
        TimelineMapOptions {
            width: u32::MAX,
            ..TimelineMapOptions::default()
        },
    )
    .unwrap();
    assert_eq!(wide.width, 1024);
    assert_bounded_opaque_image(&wide);

    for options in [
        TimelineMapOptions {
            start_seconds: f64::NAN,
            ..TimelineMapOptions::default()
        },
        TimelineMapOptions {
            start_seconds: -0.01,
            ..TimelineMapOptions::default()
        },
        TimelineMapOptions {
            end_seconds: Some(f64::INFINITY),
            ..TimelineMapOptions::default()
        },
        TimelineMapOptions {
            start_seconds: 2.0,
            end_seconds: Some(2.0),
            ..TimelineMapOptions::default()
        },
        TimelineMapOptions {
            playhead_seconds: Some(f64::NAN),
            ..TimelineMapOptions::default()
        },
    ] {
        assert!(render(&project, options).is_err());
    }
}

#[test]
fn semantic_fixture_draws_lanes_clips_marker_and_playhead() {
    let mut project = Project::new("fixture", FPS);
    let video_media = project.add_media(MediaSource::new(
        "/media/Hero Take 01.mov",
        1920,
        1080,
        FPS,
        480,
        true,
    ));
    let audio_media = project.add_media(MediaSource::new(
        "/media/interview.wav",
        0,
        0,
        FPS,
        480,
        true,
    ));

    let audio = project.add_track(TrackKind::Audio, "Dialogue");
    let main = project.add_track(TrackKind::Video, "Main Story");
    let overlay = project.add_track(TrackKind::Video, "Picture in Picture");
    let titles = project.add_track(TrackKind::Text, "Titles");

    project
        .add_clip(audio, audio_media, range(0, 144), time(0))
        .unwrap();
    project
        .add_clip(main, video_media, range(0, 96), time(0))
        .unwrap();
    project
        .add_clip(overlay, video_media, range(96, 48), time(48))
        .unwrap();
    project
        .add_generated(titles, Generator::text("Opening\nTitle"), range(24, 72))
        .unwrap();
    project
        .timeline_mut()
        .add_marker(Marker::new(time(60), "Beat A", MarkerColor::Teal))
        .unwrap();

    let image = render(
        &project,
        TimelineMapOptions {
            width: 900,
            start_seconds: 0.0,
            end_seconds: Some(8.0),
            playhead_seconds: Some(3.5),
        },
    )
    .unwrap();

    assert_bounded_opaque_image(&image);
    assert!(
        image
            .pixels
            .chunks_exact(4)
            .any(|pixel| pixel != &image.pixels[..4])
    );
    assert!(has_plot_pixel(&image, VIDEO));
    assert!(has_plot_pixel(&image, AUDIO));
    assert!(has_plot_pixel(&image, TEXT));
    assert!(has_pixel(&image, TEAL_MARKER));
    assert!(has_pixel(&image, PLAYHEAD));
}

#[test]
fn time_window_only_draws_intersecting_clip_colors_in_plot() {
    let mut project = Project::new("window", FPS);
    let video_media = project.add_media(MediaSource::new(
        "/media/inside.mov",
        1920,
        1080,
        FPS,
        480,
        false,
    ));
    let audio_media =
        project.add_media(MediaSource::new("/media/outside.wav", 0, 0, FPS, 480, true));
    let audio = project.add_track(TrackKind::Audio, "Outside");
    let video = project.add_track(TrackKind::Video, "Inside");
    project
        .add_clip(audio, audio_media, range(0, 24), time(0))
        .unwrap();
    project
        .add_clip(video, video_media, range(0, 48), time(240))
        .unwrap();

    let image = render(
        &project,
        TimelineMapOptions {
            width: 640,
            start_seconds: 10.0,
            end_seconds: Some(13.0),
            playhead_seconds: None,
        },
    )
    .unwrap();

    assert!(has_plot_pixel(&image, VIDEO));
    assert!(!has_plot_pixel(&image, AUDIO));
}

#[test]
fn many_tracks_are_omitted_but_main_lane_is_preserved_deterministically() {
    let mut project = Project::new("dense", FPS);
    let media = project.add_media(MediaSource::new(
        "/media/main.mov",
        1920,
        1080,
        FPS,
        240,
        false,
    ));
    let main = project.add_track(TrackKind::Video, "Main");
    project
        .add_clip(main, media, range(0, 96), time(0))
        .unwrap();
    for index in 0..40 {
        project.add_track(TrackKind::Text, format!("Caption Lane {index:02}"));
    }

    let options = TimelineMapOptions {
        width: 768,
        start_seconds: 0.0,
        end_seconds: Some(5.0),
        playhead_seconds: None,
    };
    let first = render(&project, options).unwrap();
    let second = render(&project, options).unwrap();

    assert_bounded_opaque_image(&first);
    assert_eq!(first.width, second.width);
    assert_eq!(first.height, second.height);
    assert_eq!(first.pixels, second.pixels);
    assert!(has_pixel(&first, OMISSION_BACKGROUND));
    assert!(
        has_plot_pixel(&first, VIDEO),
        "the main video lane must survive lane omission"
    );
    assert!(
        first
            .pixels
            .chunks_exact(4)
            .any(|pixel| pixel != &first.pixels[..4])
    );
}

#[test]
fn extreme_text_and_tiny_window_stay_inside_the_image() {
    let mut project = Project::new("text", FPS);
    let huge = format!(
        "{}\n{}\t{}",
        "very long lane ".repeat(10_000),
        "\u{1f680}".repeat(10_000),
        "tail".repeat(10_000)
    );
    let track = project.add_track(TrackKind::Text, huge.clone());
    project
        .add_generated(track, Generator::text(huge), range(0, 1))
        .unwrap();

    let image = render(
        &project,
        TimelineMapOptions {
            width: 320,
            start_seconds: 0.0,
            end_seconds: Some(0.000_001),
            playhead_seconds: Some(0.000_000_5),
        },
    )
    .unwrap();
    assert_bounded_opaque_image(&image);
    assert_eq!(
        image.pixels.len(),
        image.width as usize * image.height as usize * 4
    );
}
