//! Golden-frame tests for the effect engine (M4).
//!
//! Each effect renders a deterministic fixture frame at fixed parameters and
//! compares the readback against a stored PNG in `tests/goldens/`. Set
//! `BLESS_GOLDEN=1` to regenerate every golden after an intentional change;
//! a missing golden is written (and the test passes) so adding a case is a
//! one-run bootstrap. Tests skip gracefully when no GPU adapter is present
//! (CI without a GPU), matching the rest of the compositor suite.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cutlass_compositor::{
    CompositeLayer, Compositor, CompositorConfig, GpuContext, LayerEffect, LayerPlacement,
    RgbaImage,
};

const W: u32 = 64;
const H: u32 = 64;
/// Per-channel tolerance: GPU rasterization differs by a code or two between
/// drivers; anything past this is a real regression.
const TOL: i32 = 2;

fn try_gpu() -> Option<GpuContext> {
    GpuContext::new_headless_blocking().ok()
}

/// A deterministic fixture: a diagonal gradient with four vertical bars, so
/// blur has sharp edges to soften and vignette has bright content to darken.
fn fixture_frame() -> Arc<Vec<u8>> {
    let mut bytes = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let bar = (x / 8) % 2 == 0;
            let r = if bar { 230 } else { 30 };
            let g = (x * 255 / (W - 1)) as u8;
            let b = (y * 255 / (H - 1)) as u8;
            bytes[i..i + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    Arc::new(bytes)
}

fn render(compositor: &mut Compositor, gpu: &GpuContext, effects: Vec<LayerEffect>) -> RgbaImage {
    let config = CompositorConfig::new(W, H);
    let layer = CompositeLayer::rgba(fixture_frame(), W, H, LayerPlacement::full_canvas(&config))
        .with_effects(effects);
    compositor
        .composite(gpu, &config, &[layer])
        .expect("composite")
}

fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

fn write_png(path: &Path, image: &RgbaImage) {
    image::save_buffer(
        path,
        &image.bytes,
        image.width,
        image.height,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write golden png");
}

fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let img = image::open(path).expect("read golden png").to_rgba8();
    (img.width(), img.height(), img.into_raw())
}

fn assert_golden(name: &str, image: &RgbaImage) {
    let dir = goldens_dir();
    std::fs::create_dir_all(&dir).expect("goldens dir");
    let path = dir.join(format!("{name}.png"));
    let bless = std::env::var_os("BLESS_GOLDEN").is_some();
    if bless || !path.exists() {
        write_png(&path, image);
        if !bless {
            eprintln!("golden {name}: bootstrapped (no prior fixture)");
        }
        return;
    }
    let (gw, gh, golden) = read_png(&path);
    assert_eq!((gw, gh), (image.width, image.height), "golden {name} size");
    let mut max_diff = 0i32;
    let mut exceed = 0usize;
    for (a, b) in image.bytes.iter().zip(golden.iter()) {
        let d = (i32::from(*a) - i32::from(*b)).abs();
        max_diff = max_diff.max(d);
        if d > TOL {
            exceed += 1;
        }
    }
    assert_eq!(
        exceed, 0,
        "golden {name}: {exceed} channels exceed tolerance {TOL} (max diff {max_diff}); \
         set BLESS_GOLDEN=1 if this change is intentional"
    );
}

#[test]
fn golden_no_effects_is_the_fixture() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping golden_no_effects_is_the_fixture: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    // No effects: the placed fixture composites straight onto the canvas. This
    // pins the no-effect path against the same fixture the effect goldens
    // start from.
    let image = render(&mut compositor, &gpu, vec![]);
    assert_golden("no_effects", &image);
}

#[test]
fn golden_gaussian_blur() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping golden_gaussian_blur: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let image = render(
        &mut compositor,
        &gpu,
        vec![LayerEffect::new("gaussian_blur").with_param(0, 3.0)],
    );
    assert_golden("gaussian_blur", &image);
}

#[test]
fn golden_vignette() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping golden_vignette: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let image = render(
        &mut compositor,
        &gpu,
        vec![LayerEffect::new("vignette").with_param(0, 0.85)],
    );
    assert_golden("vignette", &image);
}

/// One golden per starter-pack effect at a representative parameter set.
macro_rules! golden_effect {
    ($test:ident, $name:literal, $effect:expr) => {
        #[test]
        fn $test() {
            let Some(gpu) = try_gpu() else {
                eprintln!(concat!("skipping ", stringify!($test), ": no GPU adapter"));
                return;
            };
            let mut compositor = Compositor::new(&gpu).expect("compositor");
            let image = render(&mut compositor, &gpu, vec![$effect]);
            assert_golden($name, &image);
        }
    };
}

golden_effect!(golden_sharpen, "sharpen", LayerEffect::new("sharpen").with_param(0, 1.0));
golden_effect!(golden_pixelate, "pixelate", LayerEffect::new("pixelate").with_param(0, 8.0));
golden_effect!(
    golden_glitch,
    "glitch",
    LayerEffect::new("glitch").with_param(0, 0.8).with_param(1, 3.0)
);
golden_effect!(
    golden_chromatic_aberration,
    "chromatic_aberration",
    LayerEffect::new("chromatic_aberration").with_param(0, 0.8)
);
golden_effect!(
    golden_grain,
    "grain",
    LayerEffect::new("grain").with_param(0, 0.5).with_param(1, 7.0)
);
golden_effect!(
    golden_glow,
    "glow",
    LayerEffect::new("glow").with_param(0, 0.5).with_param(1, 1.2)
);
golden_effect!(golden_zoom_blur, "zoom_blur", LayerEffect::new("zoom_blur").with_param(0, 0.8));
golden_effect!(golden_mirror, "mirror", LayerEffect::new("mirror").with_param(0, 0.0));

fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    [
        image.bytes[i],
        image.bytes[i + 1],
        image.bytes[i + 2],
        image.bytes[i + 3],
    ]
}

#[test]
fn vignette_darkens_corners_more_than_center() {
    // Behavioral guard independent of any committed golden: the vignette must
    // pull the corners down while the center stays close to the source.
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping vignette_darkens_corners_more_than_center: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let plain = render(&mut compositor, &gpu, vec![]);
    let vign = render(
        &mut compositor,
        &gpu,
        vec![LayerEffect::new("vignette").with_param(0, 0.85)],
    );

    let center_drop = i32::from(plain.bytes[((H / 2 * W + W / 2) * 4) as usize])
        - i32::from(vign.bytes[((H / 2 * W + W / 2) * 4) as usize]);
    let corner_plain = pixel(&plain, 1, 1);
    let corner_vign = pixel(&vign, 1, 1);
    let corner_drop = i32::from(corner_plain[0]) - i32::from(corner_vign[0]);

    assert!(center_drop.abs() <= 4, "center barely changes: {center_drop}");
    assert!(
        corner_drop > center_drop + 10,
        "corner darkens much more than center (corner {corner_drop}, center {center_drop})"
    );
    assert_eq!(corner_vign[3], 255, "output stays opaque");
}

#[test]
fn blur_softens_bar_edges() {
    // The fixture has hard vertical bars (period 8px). Blurring must reduce the
    // contrast across a bar edge.
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping blur_softens_bar_edges: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let plain = render(&mut compositor, &gpu, vec![]);
    let blurred = render(
        &mut compositor,
        &gpu,
        vec![LayerEffect::new("gaussian_blur").with_param(0, 4.0)],
    );

    // Edge between bar 0 (bright) and bar 1 (dark) is at x=8, row 32.
    let edge_contrast = |img: &RgbaImage| {
        i32::from(pixel(img, 7, 32)[0]) - i32::from(pixel(img, 9, 32)[0])
    };
    let plain_c = edge_contrast(&plain).abs();
    let blur_c = edge_contrast(&blurred).abs();
    assert!(
        blur_c < plain_c,
        "blur reduces edge contrast (plain {plain_c}, blurred {blur_c})"
    );
}

#[test]
fn mirror_reflects_the_left_half_onto_the_right() {
    // Mode 0 folds the left half across the centre: a column and its mirror
    // about x = W/2 must read the same source content.
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping mirror_reflects_the_left_half_onto_the_right: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let mirrored = render(
        &mut compositor,
        &gpu,
        vec![LayerEffect::new("mirror").with_param(0, 0.0)],
    );
    let plain = render(&mut compositor, &gpu, vec![]);
    // x=20 is in the left half; its reflection is x = W-1-20 = 43.
    let left = pixel(&plain, 20, 32);
    let reflected = pixel(&mirrored, 43, 32);
    for ch in 0..3 {
        assert!(
            (i32::from(left[ch]) - i32::from(reflected[ch])).abs() <= TOL,
            "mirror copies the left half onto the right (ch {ch}: {} vs {})",
            left[ch],
            reflected[ch]
        );
    }
}

#[test]
fn pixelate_collapses_cells_to_one_colour() {
    // With a 16px cell, all pixels inside one cell read the same source
    // centre, so they must be identical.
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping pixelate_collapses_cells_to_one_colour: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let pix = render(
        &mut compositor,
        &gpu,
        vec![LayerEffect::new("pixelate").with_param(0, 16.0)],
    );
    let a = pixel(&pix, 1, 1);
    let b = pixel(&pix, 14, 14);
    for ch in 0..4 {
        assert!(
            (i32::from(a[ch]) - i32::from(b[ch])).abs() <= TOL,
            "pixels in one cell share a colour (ch {ch}: {} vs {})",
            a[ch],
            b[ch]
        );
    }
}

#[test]
fn every_starter_effect_changes_the_frame() {
    // A guard against silently-broken (no-op) shaders: each effect, at a
    // strong parameter, must visibly differ from the unprocessed fixture.
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping every_starter_effect_changes_the_frame: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let plain = render(&mut compositor, &gpu, vec![]);

    let cases = [
        LayerEffect::new("sharpen").with_param(0, 2.0),
        LayerEffect::new("pixelate").with_param(0, 16.0),
        LayerEffect::new("glitch").with_param(0, 1.0).with_param(1, 3.0),
        LayerEffect::new("chromatic_aberration").with_param(0, 1.0),
        LayerEffect::new("grain").with_param(0, 0.8).with_param(1, 7.0),
        LayerEffect::new("glow").with_param(0, 0.4).with_param(1, 2.0),
        LayerEffect::new("zoom_blur").with_param(0, 1.0),
        LayerEffect::new("mirror").with_param(0, 0.0),
    ];
    for effect in cases {
        let id = effect.effect_id.clone();
        let out = render(&mut compositor, &gpu, vec![effect]);
        let changed = out
            .bytes
            .iter()
            .zip(plain.bytes.iter())
            .filter(|(a, b)| (i32::from(**a) - i32::from(**b)).abs() > TOL)
            .count();
        assert!(
            changed > 64,
            "effect '{id}' barely changed the frame ({changed} channels) — likely a no-op"
        );
    }
}

#[test]
fn unknown_effect_is_skipped() {
    // An effect id the registry doesn't know must be a no-op, not a panic:
    // the model layer validates ids, but the compositor stays defensive.
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping unknown_effect_is_skipped: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let plain = render(&mut compositor, &gpu, vec![]);
    let with_unknown = render(&mut compositor, &gpu, vec![LayerEffect::new("does_not_exist")]);
    // Same content (the unknown effect contributes no pass), within tolerance.
    for (a, b) in plain.bytes.iter().zip(with_unknown.bytes.iter()) {
        assert!((i32::from(*a) - i32::from(*b)).abs() <= TOL);
    }
}
