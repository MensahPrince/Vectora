//! Timeline preview: resolve layers and composite via WGPU.

use std::time::Instant;

use cutlass_compositor::{Compositor, GpuContext, Yuv420pImage};
use cutlass_cache::FrameCache;
use cutlass_models::{ClipId, ClipTransform, Generator, ModelError, Project, RationalTime};
use tracing::debug;

use crate::ColorConvertPath;
use crate::composite::{composite_canvas_size, resolve_layers};
use crate::decoder_pool::DecoderPool;
use crate::error::EngineError;
use crate::frame::RgbaFrame;
use crate::generator_raster::GeneratorRaster;

#[allow(clippy::too_many_arguments)]
pub fn get_frame(
    project: &Project,
    cache: &FrameCache,
    pool: &mut DecoderPool,
    raster: &mut GeneratorRaster,
    gpu: &GpuContext,
    compositor: &mut Compositor,
    time: RationalTime,
    color_convert: ColorConvertPath,
    override_transform: Option<(ClipId, ClipTransform)>,
    override_generator: Option<(ClipId, &Generator)>,
) -> Result<RgbaFrame, EngineError> {
    let tl_rate = project.timeline().frame_rate;
    if time.rate != tl_rate {
        return Err(ModelError::RateMismatch {
            expected: tl_rate,
            got: time.rate,
        }
        .into());
    }

    let (width, height) = composite_canvas_size(project);
    let config = cutlass_compositor::CompositorConfig::new(width, height);

    // Stage timings (playback roadmap Phase 2): resolve covers decode or
    // cache read; composite covers GPU submit + RGBA readback.
    let start = Instant::now();
    let layers = resolve_layers(
        project,
        Some(cache),
        pool,
        raster,
        time,
        0.0,
        &config,
        color_convert,
        override_transform,
        override_generator,
    )?;
    let resolve_ms = start.elapsed().as_secs_f64() * 1000.0;

    // A timeline gap isn't an error: the canvas composites bottom-up from
    // black, so zero layers is just the bare canvas. Skip the GPU
    // round-trip and hand back opaque black directly.
    if layers.is_empty() {
        return black_rgba_frame(width, height);
    }

    let start = Instant::now();
    let image = compositor
        .composite(gpu, &config, &layers)
        .map_err(|e| EngineError::Preview(e.to_string()))?;
    debug!(
        resolve_ms,
        composite_ms = start.elapsed().as_secs_f64() * 1000.0,
        tick = time.value,
        "preview frame stages"
    );

    RgbaFrame::new(image.width, image.height, image.bytes)
}

/// Warm the decode path for `time` without compositing: resolve every layer
/// (sequential decode + cache fill) and drop the pixels. Playback read-ahead
/// (roadmap Phase 2) calls this for the ticks just past the playhead while
/// the worker is idle, so a GOP boundary's decode spike is paid *before* the
/// cadence reaches it instead of hitching a frame.
pub fn prefetch_frame(
    project: &Project,
    cache: &FrameCache,
    pool: &mut DecoderPool,
    raster: &mut GeneratorRaster,
    time: RationalTime,
    color_convert: ColorConvertPath,
) -> Result<(), EngineError> {
    let (width, height) = composite_canvas_size(project);
    let config = cutlass_compositor::CompositorConfig::new(width, height);
    resolve_layers(project, Some(cache), pool, raster, time, 0.0, &config, color_convert, None, None)?;
    Ok(())
}

/// Export frame path: no disk cache; returns GPU-composited YUV420P for encode.
///
/// `anim_phase` is the sub-frame animation sampling offset in timeline
/// ticks (see [`resolve_layers`]).
#[allow(clippy::too_many_arguments)]
pub fn get_export_yuv_frame(
    project: &Project,
    pool: &mut DecoderPool,
    raster: &mut GeneratorRaster,
    gpu: &GpuContext,
    compositor: &mut Compositor,
    time: RationalTime,
    anim_phase: f32,
    color_convert: ColorConvertPath,
) -> Result<Yuv420pImage, EngineError> {
    let tl_rate = project.timeline().frame_rate;
    if time.rate != tl_rate {
        return Err(ModelError::RateMismatch {
            expected: tl_rate,
            got: time.rate,
        }
        .into());
    }

    let (width, height) = composite_canvas_size(project);
    let config = cutlass_compositor::CompositorConfig::new(width, height);
    // Export never sees a gesture override: committed project state only.
    let layers = resolve_layers(
        project,
        None,
        pool,
        raster,
        time,
        anim_phase,
        &config,
        color_convert,
        None,
        None,
    )?;

    // Same gap policy as preview: a tick no clip covers exports as black.
    if layers.is_empty() {
        return Ok(black_yuv420p(width, height));
    }

    match color_convert {
        ColorConvertPath::Gpu => compositor
            .composite_yuv420p(gpu, &config, &layers)
            .map_err(|e| EngineError::Preview(e.to_string())),
        ColorConvertPath::LegacyCpu => {
            let image = compositor
                .composite(gpu, &config, &layers)
                .map_err(|e| EngineError::Preview(e.to_string()))?;
            Ok(cutlass_compositor::legacy_rgba_to_yuv420p(
                &image.bytes,
                image.width,
                image.height,
            ))
        }
    }
}

/// Opaque black canvas — what compositing zero layers produces, without the
/// GPU submit + readback.
fn black_rgba_frame(width: u32, height: u32) -> Result<RgbaFrame, EngineError> {
    let mut bytes = vec![0u8; width as usize * height as usize * 4];
    for px in bytes.chunks_exact_mut(4) {
        px[3] = 255;
    }
    RgbaFrame::new(width, height, bytes)
}

/// Limited-range black (Y=16, U=V=128), matching what the GPU and legacy CPU
/// RGBA→YUV converters emit for RGB black.
fn black_yuv420p(width: u32, height: u32) -> Yuv420pImage {
    let (w, h) = (width as usize, height as usize);
    Yuv420pImage {
        width,
        height,
        y: vec![16; w * h],
        u: vec![128; (w / 2) * (h / 2)],
        v: vec![128; (w / 2) * (h / 2)],
    }
}
