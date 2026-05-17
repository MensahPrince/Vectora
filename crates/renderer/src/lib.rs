//! Offscreen **YUV420P / NV12 / RGBA8 → RGBA8** rendering via [`wgpu`].
//!
//! Design notes and roadmap: `docs/renderer/research.md`, `docs/renderer/roadmap.md`.
//!
//! # Overview
//! - [`Renderer::new`] creates a [`wgpu::Device`] + [`wgpu::Queue`] and three pipelines (one per pixel format).
//! - [`Renderer::render`] accepts a **single** [`Layer`] (MVP) and draws full-bleed into a caller-owned [`RenderTarget`].
//! - [`Renderer::read_pixels_rgba8`] copies the RGBA target back to the CPU (tests / export hooks).

mod error;
mod gpu;
mod layer;
mod pixel_format;
mod target;
mod upload;

pub use error::RendererError;
pub use gpu::{upload_decoded_frame_for_test, Renderer};
pub use layer::{Layer, Transform};
pub use target::RenderTarget;

use tracing::info;

/// Smoke hook for the app crate (logging).
pub fn log_name() {
    info!(target: "cutlass_renderer", "renderer");
}
