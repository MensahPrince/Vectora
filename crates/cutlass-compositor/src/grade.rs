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

    /// CPU mirror of `grade.wgsl`'s `apply_color_grade`, kept in lockstep
    /// with the shader (the GPU render tests compare against this). Used to
    /// bake grade recipes into `.cube` LUTs and as the reference for
    /// tolerance assertions.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mut c = rgb;
        let exposure = 2f32.powf(2.0 * self.exposure);
        for ch in &mut c {
            *ch *= exposure;
        }
        c[0] += 0.25 * self.temperature;
        c[2] -= 0.25 * self.temperature;
        c[1] += 0.25 * self.tint;
        for ch in &mut c {
            *ch += 0.25 * self.brightness;
            *ch = (*ch - 0.5) * (1.0 + self.contrast) + 0.5;
        }
        let luma = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let sat = 1.0 + self.saturation;
        c.map(|ch| (luma + (ch - luma) * sat).clamp(0.0, 1.0))
    }
}
