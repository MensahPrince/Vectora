use decoder::DecodedVideoFrame;

/// 2D placement in **target pixel space** (used by future multi-layer compositing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub translate: [f32; 2],
    pub scale: [f32; 2],
    pub rotate_radians: f32,
}

impl Transform {
    pub fn identity() -> Self {
        Self {
            translate: [0.0, 0.0],
            scale: [1.0, 1.0],
            rotate_radians: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Transform;

    #[test]
    fn transform_identity_values() {
        let t = Transform::identity();
        assert_eq!(t.translate, [0.0, 0.0]);
        assert_eq!(t.scale, [1.0, 1.0]);
        assert_eq!(t.rotate_radians, 0.0);
    }
}

/// One video layer. MVP renders a single layer full-bleed; `transform` and `opacity` are ignored.
#[derive(Debug, Clone)]
pub struct Layer {
    pub frame: DecodedVideoFrame,
    pub transform: Transform,
    pub opacity: f32,
}
