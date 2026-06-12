/// Canvas dimensions + background for a composite pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositorConfig {
    pub width: u32,
    pub height: u32,
    /// Opaque background color (`[r, g, b]`) the canvas clears to before
    /// layers composite over it.
    pub background: [u8; 3],
}

impl CompositorConfig {
    pub const fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            background: [0, 0, 0],
        }
    }

    pub const fn with_background(mut self, background: [u8; 3]) -> Self {
        self.background = background;
        self
    }
}

use std::sync::Arc;

use crate::yuv::Yuv420pLayer;

/// Where a layer's content lands on the canvas, in canvas pixels.
///
/// The compositor draws a quad of `size` centered on `center`, rotated by
/// `rotation`, with content alpha multiplied by `opacity`. The same values
/// drive preview hit-testing, so picking can never disagree with rendering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerPlacement {
    /// Content center in canvas pixels (+x right, +y down).
    pub center: [f32; 2],
    /// Pre-rotation content extent (width, height) in canvas pixels.
    pub size: [f32; 2],
    /// Clockwise rotation about the center, in radians.
    pub rotation: f32,
    /// Layer opacity, 0.0..=1.0; multiplies the content's alpha.
    pub opacity: f32,
}

impl LayerPlacement {
    /// The pre-transform behavior: content stretched over the whole canvas.
    pub fn full_canvas(config: &CompositorConfig) -> Self {
        Self {
            center: [config.width as f32 / 2.0, config.height as f32 / 2.0],
            size: [config.width as f32, config.height as f32],
            rotation: 0.0,
            opacity: 1.0,
        }
    }
}

/// Content UV rect covering the whole texture (no crop, no mirroring).
pub const FULL_UV: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// One layer in bottom-to-top stacking order: pixel content plus where it
/// lands on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositeLayer {
    pub content: LayerContent,
    pub placement: LayerPlacement,
    /// Content UV rect `[u0, v0, u1, v1]` sampled across the placed quad:
    /// `(u0, v0)` lands on the quad's top-left corner, `(u1, v1)` on the
    /// bottom-right. A sub-rect crops; a reversed axis (`u0 > u1` or
    /// `v0 > v1`) mirrors. Ignored by solid fills.
    pub uv: [f32; 4],
}

impl CompositeLayer {
    pub fn yuv420p(layer: Yuv420pLayer, placement: LayerPlacement) -> Self {
        Self {
            content: LayerContent::Yuv420p(layer),
            placement,
            uv: FULL_UV,
        }
    }

    pub fn rgba(bytes: Arc<Vec<u8>>, width: u32, height: u32, placement: LayerPlacement) -> Self {
        Self {
            content: LayerContent::Rgba {
                bytes,
                width,
                height,
            },
            placement,
            uv: FULL_UV,
        }
    }

    pub fn solid(rgba: [u8; 4], placement: LayerPlacement) -> Self {
        Self {
            content: LayerContent::Solid { rgba },
            placement,
            uv: FULL_UV,
        }
    }

    /// Replace the sampled UV rect (crop / mirror).
    pub fn with_uv(mut self, uv: [f32; 4]) -> Self {
        self.uv = uv;
        self
    }
}

/// Pixel source for a [`CompositeLayer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerContent {
    /// Decoder-native YUV420P; converted and scaled on GPU.
    Yuv420p(Yuv420pLayer),
    /// RGBA8 (width×height×4) from CPU conversion or generator
    /// rasterization. Shared via `Arc` so cached rasters (text, shapes) ride
    /// the per-frame composite path without a copy.
    Rgba {
        bytes: Arc<Vec<u8>>,
        width: u32,
        height: u32,
    },
    /// Solid fill (RGBA 0–255) across the placed quad.
    Solid { rgba: [u8; 4] },
}
