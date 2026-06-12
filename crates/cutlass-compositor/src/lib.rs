//! WGPU frame compositor for Cutlass preview and export.
//!
//! Layers are composited **bottom-to-top** with src-over alpha blending.
//! YUV420P media layers are converted (and scaled) on GPU via `yuv_blit.wgsl`;
//! export readback uses `rgba_to_yuv.wgsl`. CPU helpers in [`legacy_rgba_to_yuv420p`]
//! remain for tests and the engine's legacy CPU fallback path.
//! [`GpuContext::new_headless_blocking`] is the default entry point for engine
//! and tests. Future Slint UI should create one shared [`GpuContext`] and pass
//! it to both Slint (`WGPUConfiguration::Manual`) and [`Compositor::new`].

mod compositor;
mod error;
mod gpu;
mod image;
mod layer;
mod yuv;

pub use compositor::Compositor;
pub use error::CompositorError;
pub use gpu::GpuContext;
pub use image::RgbaImage;
pub use layer::{CompositeLayer, CompositorConfig, FULL_UV, LayerContent, LayerPlacement};
pub use yuv::{Yuv420pImage, Yuv420pLayer, legacy_rgba_to_yuv420p};

use tracing::info;

pub fn init() {
    info!("cutlass-compositor ready");
}
