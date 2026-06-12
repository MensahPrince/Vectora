//! GPU compositor throughput (requires an adapter; skips silently if none).
//!
//! Run: `cargo bench -p cutlass-compositor --bench composite`

use cutlass_compositor::{
    CompositeLayer, Compositor, CompositorConfig, GpuContext, LayerEffect, LayerPlacement,
};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

const W: u32 = 1920;
const H: u32 = 1080;

struct GpuBench {
    gpu: GpuContext,
    compositor: Compositor,
}

impl GpuBench {
    fn try_new() -> Option<Self> {
        let gpu = GpuContext::new_headless_blocking().ok()?;
        let compositor = Compositor::new(&gpu).ok()?;
        Some(Self { gpu, compositor })
    }
}

fn solid_bytes(rgba: [u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0u8; (W * H * 4) as usize];
    for chunk in bytes.chunks_exact_mut(4) {
        chunk.copy_from_slice(&rgba);
    }
    bytes
}

fn bench_solid(c: &mut Criterion) {
    let Some(mut ctx) = GpuBench::try_new() else {
        eprintln!("composite bench: no GPU adapter, skipping");
        return;
    };
    let config = CompositorConfig::new(W, H);
    let layers = [CompositeLayer::solid(
        [200, 40, 10, 255],
        LayerPlacement::full_canvas(&config),
    )];

    let mut group = c.benchmark_group("compositor/solid");
    group.throughput(Throughput::Bytes((W * H * 4) as u64));
    group.bench_function("1080p_readback", |b| {
        b.iter(|| {
            ctx.compositor
                .composite(&ctx.gpu, &config, &layers)
                .expect("composite")
        });
    });
    group.finish();
}

fn bench_rgba_layer(c: &mut Criterion) {
    let Some(mut ctx) = GpuBench::try_new() else {
        return;
    };
    let config = CompositorConfig::new(W, H);
    let bytes = std::sync::Arc::new(solid_bytes([10, 120, 200, 255]));
    let layers = [CompositeLayer::rgba(
        bytes,
        W,
        H,
        LayerPlacement::full_canvas(&config),
    )];

    let mut group = c.benchmark_group("compositor/rgba");
    group.throughput(Throughput::Bytes((W * H * 4) as u64));
    group.bench_function("1080p_upload_blend_readback", |b| {
        b.iter(|| {
            ctx.compositor
                .composite(&ctx.gpu, &config, &layers)
                .expect("composite")
        });
    });
    group.finish();
}

fn bench_two_layers(c: &mut Criterion) {
    let Some(mut ctx) = GpuBench::try_new() else {
        return;
    };
    let config = CompositorConfig::new(W, H);
    let top = std::sync::Arc::new(solid_bytes([0, 255, 0, 128]));
    let layers = [
        CompositeLayer::solid([255, 0, 0, 255], LayerPlacement::full_canvas(&config)),
        CompositeLayer::rgba(top, W, H, LayerPlacement::full_canvas(&config)),
    ];

    let mut group = c.benchmark_group("compositor/stack");
    group.throughput(Throughput::Bytes((W * H * 4) as u64));
    group.bench_function("solid_plus_rgba_1080p", |b| {
        b.iter(|| {
            ctx.compositor
                .composite(&ctx.gpu, &config, &layers)
                .expect("composite")
        });
    });
    group.finish();
}

fn bench_effect(c: &mut Criterion, name: &str, effects: Vec<LayerEffect>) {
    let Some(mut ctx) = GpuBench::try_new() else {
        return;
    };
    let config = CompositorConfig::new(W, H);
    let bytes = std::sync::Arc::new(solid_bytes([10, 120, 200, 255]));
    let layers = [
        CompositeLayer::rgba(bytes, W, H, LayerPlacement::full_canvas(&config)).with_effects(effects),
    ];

    let mut group = c.benchmark_group("compositor/effect");
    group.throughput(Throughput::Bytes((W * H * 4) as u64));
    group.bench_function(format!("{name}_1080p"), |b| {
        b.iter(|| {
            ctx.compositor
                .composite(&ctx.gpu, &config, &layers)
                .expect("composite")
        });
    });
    group.finish();
}

fn bench_gaussian_blur(c: &mut Criterion) {
    bench_effect(
        c,
        "gaussian_blur",
        vec![LayerEffect::new("gaussian_blur").with_param(0, 4.0)],
    );
}

fn bench_vignette(c: &mut Criterion) {
    bench_effect(
        c,
        "vignette",
        vec![LayerEffect::new("vignette").with_param(0, 0.8)],
    );
}

criterion_group!(
    benches,
    bench_solid,
    bench_rgba_layer,
    bench_two_layers,
    bench_gaussian_blur,
    bench_vignette
);
criterion_main!(benches);
