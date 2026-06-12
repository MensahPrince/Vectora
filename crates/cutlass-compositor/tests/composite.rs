use cutlass_compositor::{
    CompositeLayer, Compositor, CompositorConfig, CompositorError, GpuContext, LayerPlacement,
};

fn try_gpu() -> Option<GpuContext> {
    GpuContext::new_headless_blocking().ok()
}

fn solid_canvas(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0u8; (width * height * 4) as usize];
    for chunk in bytes.chunks_exact_mut(4) {
        chunk.copy_from_slice(&rgba);
    }
    bytes
}

#[test]
fn solid_fills_canvas() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping solid_fills_canvas: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(64, 64);
    let image = compositor
        .composite(
            &gpu,
            &config,
            &[CompositeLayer::solid(
                [200, 40, 10, 255],
                LayerPlacement::full_canvas(&config),
            )],
        )
        .expect("composite");

    assert_eq!(image.width, 64);
    assert_eq!(image.height, 64);
    assert!(image.bytes.chunks_exact(4).all(|p| p == [200, 40, 10, 255]));
}

#[test]
fn background_color_fills_uncovered_canvas() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping background_color_fills_uncovered_canvas: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(8, 8).with_background([10, 120, 250]);

    // A half-width layer leaves the right side of the canvas bare: the
    // bare half clears to the background, the covered half stays content.
    let image = compositor
        .composite(
            &gpu,
            &config,
            &[CompositeLayer::solid(
                [255, 0, 0, 255],
                LayerPlacement {
                    center: [2.0, 4.0],
                    size: [4.0, 8.0],
                    rotation: 0.0,
                    opacity: 1.0,
                },
            )],
        )
        .expect("composite");

    assert_eq!(pixel(&image, 1, 4), [255, 0, 0, 255], "covered: content");
    assert_eq!(pixel(&image, 6, 4), [10, 120, 250, 255], "bare: background");

    // Zero layers = the bare canvas, all background.
    let empty = compositor.composite(&gpu, &config, &[]).expect("composite");
    assert!(empty.bytes.chunks_exact(4).all(|p| p == [10, 120, 250, 255]));
}

#[test]
fn rgba_over_solid_alpha_blends() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping rgba_over_solid_alpha_blends: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(4, 4);

    let top = solid_canvas(4, 4, [0, 255, 0, 128]);
    let image = compositor
        .composite(
            &gpu,
            &config,
            &[
                CompositeLayer::solid([255, 0, 0, 255], LayerPlacement::full_canvas(&config)),
                CompositeLayer::rgba(
                    std::sync::Arc::new(top),
                    4,
                    4,
                    LayerPlacement::full_canvas(&config),
                ),
            ],
        )
        .expect("composite");

    let pixel = |x: u32, y: u32| {
        let i = ((y * 4 + x) * 4) as usize;
        [
            image.bytes[i],
            image.bytes[i + 1],
            image.bytes[i + 2],
            image.bytes[i + 3],
        ]
    };

    let p = pixel(1, 1);
    assert!(p[0] > 100 && p[0] < 200, "red channel blended: {p:?}");
    assert!(p[1] > 100, "green channel present: {p:?}");
    assert_eq!(p[3], 255, "opaque output alpha");
}

fn pixel(image: &cutlass_compositor::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    [
        image.bytes[i],
        image.bytes[i + 1],
        image.bytes[i + 2],
        image.bytes[i + 3],
    ]
}

#[test]
fn uv_rect_crops_and_mirrors_sampled_content() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping uv_rect_crops_and_mirrors_sampled_content: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(8, 8);

    // Content: left half red, right half blue.
    let mut bytes = vec![0u8; 8 * 8 * 4];
    for y in 0..8u32 {
        for x in 0..8u32 {
            let i = ((y * 8 + x) * 4) as usize;
            let color = if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] };
            bytes[i..i + 4].copy_from_slice(&color);
        }
    }
    let bytes = std::sync::Arc::new(bytes);
    let full = LayerPlacement::full_canvas(&config);

    // Crop to the right half: the whole canvas samples blue.
    let cropped = compositor
        .composite(
            &gpu,
            &config,
            &[CompositeLayer::rgba(bytes.clone(), 8, 8, full).with_uv([0.5, 0.0, 1.0, 1.0])],
        )
        .expect("composite");
    assert_eq!(pixel(&cropped, 1, 4)[2], 255, "left of canvas shows cropped blue");
    assert_eq!(pixel(&cropped, 6, 4)[2], 255, "right of canvas shows cropped blue");

    // Horizontal mirror: red lands on the right, blue on the left.
    let flipped = compositor
        .composite(
            &gpu,
            &config,
            &[CompositeLayer::rgba(bytes, 8, 8, full).with_uv([1.0, 0.0, 0.0, 1.0])],
        )
        .expect("composite");
    assert_eq!(pixel(&flipped, 1, 4)[2], 255, "blue mirrored to the left");
    assert_eq!(pixel(&flipped, 6, 4)[0], 255, "red mirrored to the right");
}

#[test]
fn placed_solid_covers_only_its_rect() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping placed_solid_covers_only_its_rect: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(64, 64);
    // 16×16 red square centered at (16, 16): covers [8, 24) × [8, 24).
    let image = compositor
        .composite(
            &gpu,
            &config,
            &[CompositeLayer::solid(
                [255, 0, 0, 255],
                LayerPlacement {
                    center: [16.0, 16.0],
                    size: [16.0, 16.0],
                    rotation: 0.0,
                    opacity: 1.0,
                },
            )],
        )
        .expect("composite");

    assert_eq!(pixel(&image, 16, 16), [255, 0, 0, 255], "inside the rect");
    assert_eq!(pixel(&image, 9, 9), [255, 0, 0, 255], "near top-left corner");
    assert_eq!(pixel(&image, 48, 48), [0, 0, 0, 255], "outside is canvas black");
    assert_eq!(pixel(&image, 32, 16), [0, 0, 0, 255], "right of the rect");
}

#[test]
fn rotated_quad_lands_rotated() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping rotated_quad_lands_rotated: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(64, 64);
    // A 48×8 horizontal strip at the canvas center, rotated 90°: becomes a
    // vertical strip (x 28..36, y 8..56).
    let image = compositor
        .composite(
            &gpu,
            &config,
            &[CompositeLayer::solid(
                [0, 255, 0, 255],
                LayerPlacement {
                    center: [32.0, 32.0],
                    size: [48.0, 8.0],
                    rotation: std::f32::consts::FRAC_PI_2,
                    opacity: 1.0,
                },
            )],
        )
        .expect("composite");

    assert_eq!(pixel(&image, 32, 12), [0, 255, 0, 255], "on the vertical strip");
    assert_eq!(pixel(&image, 32, 52), [0, 255, 0, 255], "on the vertical strip");
    assert_eq!(pixel(&image, 12, 32), [0, 0, 0, 255], "horizontal extent gone");
    assert_eq!(pixel(&image, 52, 32), [0, 0, 0, 255], "horizontal extent gone");
}

#[test]
fn layer_opacity_blends_over_lower_layer() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping layer_opacity_blends_over_lower_layer: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(8, 8);
    let image = compositor
        .composite(
            &gpu,
            &config,
            &[
                CompositeLayer::solid([0, 0, 0, 255], LayerPlacement::full_canvas(&config)),
                CompositeLayer::solid(
                    [255, 255, 255, 255],
                    LayerPlacement {
                        opacity: 0.5,
                        ..LayerPlacement::full_canvas(&config)
                    },
                ),
            ],
        )
        .expect("composite");

    let p = pixel(&image, 4, 4);
    assert!(
        (120..=135).contains(&p[0]) && p[0] == p[1] && p[1] == p[2],
        "half-opacity white over black is mid gray: {p:?}"
    );
    assert_eq!(p[3], 255, "output stays opaque");
}

#[test]
fn two_solid_layers_keep_their_own_colors() {
    // Regression: a single reused uniform buffer made every solid in a pass
    // render with the last-written color.
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping two_solid_layers_keep_their_own_colors: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(64, 64);
    let half = LayerPlacement {
        center: [16.0, 32.0],
        size: [32.0, 64.0],
        rotation: 0.0,
        opacity: 1.0,
    };
    let other_half = LayerPlacement {
        center: [48.0, 32.0],
        ..half
    };
    let image = compositor
        .composite(
            &gpu,
            &config,
            &[
                CompositeLayer::solid([255, 0, 0, 255], half),
                CompositeLayer::solid([0, 0, 255, 255], other_half),
            ],
        )
        .expect("composite");

    assert_eq!(pixel(&image, 8, 32), [255, 0, 0, 255], "left half red");
    assert_eq!(pixel(&image, 56, 32), [0, 0, 255, 255], "right half blue");
}

#[test]
fn empty_layers_yields_opaque_black() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skipping empty_layers_yields_opaque_black: no GPU adapter");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(8, 8);
    let image = compositor
        .composite(&gpu, &config, &[])
        .expect("composite");

    // The canvas clears to opaque black: placed layers may leave parts of it
    // uncovered, and preview/export define the background as black.
    assert!(image.bytes.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
}

#[test]
fn invalid_dimensions_error() {
    let Some(gpu) = try_gpu() else {
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let err = compositor
        .composite(&gpu, &CompositorConfig::new(0, 64), &[])
        .unwrap_err();
    assert!(matches!(err, CompositorError::InvalidDimensions { .. }));
}
