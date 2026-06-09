//! Timeline preview: resolve layers and composite via WGPU.

use cutlass_compositor::{Compositor, GpuContext, Yuv420pImage};
use cutlass_cache::FrameCache;
use cutlass_models::{ModelError, Project, RationalTime};

use crate::ColorConvertPath;
use crate::composite::{composite_canvas_size, resolve_layers};
use crate::decoder_pool::DecoderPool;
use crate::error::EngineError;
use crate::frame::RgbaFrame;

pub fn get_frame(
    project: &Project,
    cache: &FrameCache,
    pool: &mut DecoderPool,
    gpu: &GpuContext,
    compositor: &mut Compositor,
    time: RationalTime,
    color_convert: ColorConvertPath,
) -> Result<RgbaFrame, EngineError> {
    let (width, height) = composite_canvas_size(project);
    let config = cutlass_compositor::CompositorConfig::new(width, height);
    let layers = resolve_layers(project, Some(cache), pool, time, &config, color_convert)?;

    if layers.is_empty() {
        return Err(EngineError::Preview("no video at timeline position".into()));
    }

    let tl_rate = project.timeline().frame_rate;
    if time.rate != tl_rate {
        return Err(ModelError::RateMismatch {
            expected: tl_rate,
            got: time.rate,
        }
        .into());
    }

    let image = compositor
        .composite(gpu, &config, &layers)
        .map_err(|e| EngineError::Preview(e.to_string()))?;

    RgbaFrame::new(image.width, image.height, image.bytes)
}

/// Export frame path: no disk cache; returns GPU-composited YUV420P for encode.
pub fn get_export_yuv_frame(
    project: &Project,
    pool: &mut DecoderPool,
    gpu: &GpuContext,
    compositor: &mut Compositor,
    time: RationalTime,
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
    let layers = resolve_layers(project, None, pool, time, &config, color_convert)?;

    if layers.is_empty() {
        return Err(EngineError::Preview("no video at timeline position".into()));
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
