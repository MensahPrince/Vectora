//! Resolved per-layer color grade parameters consumed by the GPU shaders.
//!
//! The render bridge maps persisted [`cutlass_models::ColorAdjustments`] and
//! filter presets into this compact form. Per-clip effects, masks, and canvas
//! passes use adjacent compositor plumbing, but share this grade representation.

/// Manual color grade + filter-preset strengths, ready for the WGSL uniform block.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColorGrade {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub exposure: f32,
    pub temperature: f32,
    pub tint: f32,
}

impl ColorGrade {
    /// Identity grade: all controls are neutral and the shader output is unchanged.
    pub const IDENTITY: Self = Self {
        brightness: 0.0,
        contrast: 0.0,
        saturation: 0.0,
        exposure: 0.0,
        temperature: 0.0,
        tint: 0.0,
    };

    /// True when every slider sits at neutral, so grade work can be skipped.
    pub const fn is_identity(&self) -> bool {
        self.brightness == 0.0
            && self.contrast == 0.0
            && self.saturation == 0.0
            && self.exposure == 0.0
            && self.temperature == 0.0
            && self.tint == 0.0
    }
}
