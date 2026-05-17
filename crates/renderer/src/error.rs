use decoder::PixelFormat;

/// Errors from [`crate::Renderer`].
#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    #[error("failed to acquire wgpu adapter (no compatible GPU)")]
    NoAdapter,

    #[error("wgpu device request failed")]
    DeviceRequest(#[from] wgpu::RequestDeviceError),

    #[error("unsupported pixel format for rendering: {0:?}")]
    UnsupportedFormat(PixelFormat),

    #[error("only CPU-backed frames are supported")]
    NonCpuFrame,

    #[error("unsupported layer count for MVP: {count} (expected 1)")]
    UnsupportedLayerCount { count: usize },

    #[error("zero-dimension frame or target")]
    ZeroDimension,

    #[error("render target size {target_w}x{target_h} does not match frame {frame_w}x{frame_h}")]
    TargetSizeMismatch {
        target_w: u32,
        target_h: u32,
        frame_w: u32,
        frame_h: u32,
    },

    #[error("decoder plane layout does not match pixel format")]
    BadFrameLayout,

    #[error("readback failed: {0}")]
    Readback(String),
}

#[cfg(test)]
mod tests {
    use super::RendererError;
    use decoder::PixelFormat;

    #[test]
    fn display_messages_are_nonempty() {
        assert!(!RendererError::NoAdapter.to_string().is_empty());
        assert!(!RendererError::UnsupportedFormat(PixelFormat::Rgba8)
            .to_string()
            .is_empty());
        assert!(!RendererError::UnsupportedLayerCount { count: 3 }
            .to_string()
            .is_empty());
    }

    #[test]
    fn target_size_mismatch_lists_all_four_dimensions() {
        let s = RendererError::TargetSizeMismatch {
            target_w: 640,
            target_h: 480,
            frame_w: 320,
            frame_h: 240,
        }
        .to_string();
        assert!(s.contains("640"));
        assert!(s.contains("480"));
        assert!(s.contains("320"));
        assert!(s.contains("240"));
    }

    #[test]
    fn bad_frame_layout_and_non_cpu_have_distinct_messages() {
        assert_ne!(
            RendererError::BadFrameLayout.to_string(),
            RendererError::NonCpuFrame.to_string()
        );
    }
}
