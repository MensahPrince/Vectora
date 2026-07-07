//! Resolved per-layer color grade parameters consumed by the GPU shaders.
//!
//! The render bridge maps persisted [`cutlass_models::ColorAdjustments`] and
//! filter presets into this compact form. Additional per-clip passes (mask,
//! chroma key, effects) can extend the layer-effects plumbing alongside this.

/// Manual color grade + filter-preset strengths, ready for the WGSL uniform block.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColorGrade {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub exposure: f32,
    pub temperature: f32,
}

impl ColorGrade {
    /// True when every slider sits at neutral — skip grade math in the shader.
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }
}
