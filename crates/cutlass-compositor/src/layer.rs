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

/// A GPU effect applied to a single layer, with its parameters already
/// resolved to scalars (the engine samples animated `Param`s at the frame
/// tick before building the layer — the compositor never sees a curve).
///
/// `params` are packed in the effect's declared slot order (see
/// [`crate::effects::effect_param_index`]); unused slots stay zero. An
/// `effect_id` the registry doesn't know is skipped.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerEffect {
    pub effect_id: String,
    pub params: [f32; crate::effects::EFFECT_PARAM_SLOTS],
}

impl LayerEffect {
    pub fn new(effect_id: impl Into<String>) -> Self {
        Self {
            effect_id: effect_id.into(),
            params: [0.0; crate::effects::EFFECT_PARAM_SLOTS],
        }
    }

    /// Set one parameter slot (builder style).
    pub fn with_param(mut self, slot: usize, value: f32) -> Self {
        if slot < self.params.len() {
            self.params[slot] = value;
        }
        self
    }
}

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
    /// Per-layer effect chain, applied in order before the layer composites
    /// onto the canvas. Empty for the common case (the no-effect render path
    /// stays a single pass).
    pub effects: Vec<LayerEffect>,
}

impl CompositeLayer {
    pub fn yuv420p(layer: Yuv420pLayer, placement: LayerPlacement) -> Self {
        Self {
            content: LayerContent::Yuv420p(layer),
            placement,
            uv: FULL_UV,
            effects: Vec::new(),
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
            effects: Vec::new(),
        }
    }

    pub fn solid(rgba: [u8; 4], placement: LayerPlacement) -> Self {
        Self {
            content: LayerContent::Solid { rgba },
            placement,
            uv: FULL_UV,
            effects: Vec::new(),
        }
    }

    /// An adjustment layer: its `effects` chain applies to the accumulated
    /// canvas below it (CapCut semantics), not to any content of its own. The
    /// placement opacity scales how strongly the adjusted result replaces the
    /// canvas. With an empty chain it is a no-op.
    pub fn adjustment(effects: Vec<LayerEffect>, opacity: f32) -> Self {
        Self {
            content: LayerContent::Adjustment,
            placement: LayerPlacement {
                center: [0.0, 0.0],
                size: [0.0, 0.0],
                rotation: 0.0,
                opacity,
            },
            uv: FULL_UV,
            effects,
        }
    }

    /// Replace the sampled UV rect (crop / mirror).
    pub fn with_uv(mut self, uv: [f32; 4]) -> Self {
        self.uv = uv;
        self
    }

    /// Attach an effect chain (applied in order before compositing).
    pub fn with_effects(mut self, effects: Vec<LayerEffect>) -> Self {
        self.effects = effects;
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
    /// An adjustment marker: no pixels of its own. The compositor applies the
    /// layer's effect chain to the accumulated canvas below it.
    Adjustment,
}
