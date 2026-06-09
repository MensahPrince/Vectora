//! GPU YUV ↔ RGBA conversion vs legacy CPU reference.

use cutlass_compositor::{
    CompositeLayer, Compositor, CompositorConfig, GpuContext, Yuv420pLayer,
    legacy_rgba_to_yuv420p,
};
fn try_gpu() -> Option<GpuContext> {
    GpuContext::new_headless_blocking().ok()
}

fn solid_yuv420p(width: u32, height: u32, y: u8, u: u8, v: u8) -> Yuv420pLayer {
    let w = width as usize;
    let h = height as usize;
    Yuv420pLayer::new(
        width,
        height,
        vec![y; w * h],
        width,
        vec![u; (w / 2) * (h / 2)],
        (width / 2) as u32,
        vec![v; (w / 2) * (h / 2)],
        (width / 2) as u32,
    )
}

fn legacy_yuv_to_rgba(layer: &Yuv420pLayer) -> Vec<u8> {
    let w = layer.width as usize;
    let h = layer.height as usize;
    let y_stride = layer.y_stride as usize;
    let u_stride = layer.u_stride as usize;
    let v_stride = layer.v_stride as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for row in 0..h {
        for col in 0..w {
            let yv = i32::from(layer.y[row * y_stride + col]);
            let u = i32::from(layer.u[(row / 2) * u_stride + (col / 2)]) - 128;
            let v = i32::from(layer.v[(row / 2) * v_stride + (col / 2)]) - 128;
            let r = ((298 * (yv - 16) + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * (yv - 16) - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * (yv - 16) + 516 * u + 128) >> 8).clamp(0, 255) as u8;
            let i = (row * w + col) * 4;
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 255;
        }
    }
    rgba
}

#[test]
fn gpu_yuv_gray_matches_legacy_cpu_at_same_size() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skip: no GPU");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let layer = solid_yuv420p(64, 64, 128, 128, 128);
    let legacy = legacy_yuv_to_rgba(&layer);

    let image = compositor
        .composite(
            &gpu,
            &CompositorConfig::new(64, 64),
            &[CompositeLayer::Yuv420p(layer)],
        )
        .expect("gpu composite");

    for (gpu_px, cpu_px) in image.bytes.chunks_exact(4).zip(legacy.chunks_exact(4)) {
        for ch in 0..3 {
            let diff = i16::from(gpu_px[ch]) - i16::from(cpu_px[ch]);
            assert!(
                diff.abs() <= 2,
                "channel {ch} gpu={} cpu={}",
                gpu_px[ch],
                cpu_px[ch]
            );
        }
        assert_eq!(gpu_px[3], 255);
    }
}

#[test]
fn gpu_rgba_to_yuv_matches_legacy_cpu() {
    let Some(gpu) = try_gpu() else {
        eprintln!("skip: no GPU");
        return;
    };
    let mut compositor = Compositor::new(&gpu).expect("compositor");
    let config = CompositorConfig::new(64, 64);
    let mut rgba = vec![0u8; 64 * 64 * 4];
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[180, 40, 200, 255]);
    }
    let legacy = legacy_rgba_to_yuv420p(&rgba, 64, 64);

    let gpu_yuv = compositor
        .composite_yuv420p(
            &gpu,
            &config,
            &[
                CompositeLayer::Solid {
                    rgba: [180, 40, 200, 255],
                },
            ],
        )
        .expect("gpu yuv readback");

    assert_eq!(gpu_yuv.width, legacy.width);
    assert_eq!(gpu_yuv.height, legacy.height);
    for (plane_gpu, plane_cpu) in [
        (&gpu_yuv.y, &legacy.y),
        (&gpu_yuv.u, &legacy.u),
        (&gpu_yuv.v, &legacy.v),
    ] {
        for (g, c) in plane_gpu.iter().zip(plane_cpu.iter()) {
            let diff = i16::from(*g) - i16::from(*c);
            assert!(diff.abs() <= 2, "gpu={g} cpu={c}");
        }
    }
}
