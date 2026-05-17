use decoder::PixelFormat;

use crate::error::RendererError;

pub(crate) fn plane_count_matches(fmt: PixelFormat, planes: usize) -> bool {
    fmt.plane_count() == planes
}

/// Effective byte width of one row for GPU upload (decoder layout → tightly packed row bytes).
pub(crate) fn row_bytes_per_plane(
    fmt: PixelFormat,
    frame_width: u32,
    frame_height: u32,
    plane_index: usize,
) -> Result<u32, RendererError> {
    if frame_width == 0 || frame_height == 0 {
        return Err(RendererError::ZeroDimension);
    }
    let heights = PixelFormat::plane_heights_full(fmt, frame_height)
        .map_err(|_| RendererError::BadFrameLayout)?;
    if plane_index >= heights.len() {
        return Err(RendererError::BadFrameLayout);
    }
    match fmt {
        PixelFormat::Yuv420p => Ok(match plane_index {
            0 => frame_width,
            1 | 2 => frame_width / 2,
            _ => return Err(RendererError::BadFrameLayout),
        }),
        PixelFormat::Nv12 => Ok(match plane_index {
            0 => frame_width,
            1 => frame_width,
            _ => return Err(RendererError::BadFrameLayout),
        }),
        PixelFormat::Rgba8 => Ok(match plane_index {
            0 => frame_width.checked_mul(4).ok_or(RendererError::BadFrameLayout)?,
            _ => return Err(RendererError::BadFrameLayout),
        }),
        _ => Err(RendererError::UnsupportedFormat(fmt)),
    }
}

/// Texture extent for each plane after upload.
pub(crate) fn plane_extent(
    fmt: PixelFormat,
    frame_width: u32,
    frame_height: u32,
    plane_index: usize,
) -> Result<(u32, u32), RendererError> {
    if frame_width == 0 || frame_height == 0 {
        return Err(RendererError::ZeroDimension);
    }
    let heights = PixelFormat::plane_heights_full(fmt, frame_height)
        .map_err(|_| RendererError::BadFrameLayout)?;
    if plane_index >= heights.len() {
        return Err(RendererError::BadFrameLayout);
    }
    let h = heights[plane_index];
    let (w, gpu_h) = match fmt {
        PixelFormat::Yuv420p => {
            let w = match plane_index {
                0 => frame_width,
                1 | 2 => frame_width / 2,
                _ => return Err(RendererError::BadFrameLayout),
            };
            (w, h)
        }
        PixelFormat::Nv12 => match plane_index {
            0 => (frame_width, h),
            1 => (frame_width / 2, h),
            _ => return Err(RendererError::BadFrameLayout),
        },
        PixelFormat::Rgba8 => match plane_index {
            0 => (frame_width, h),
            _ => return Err(RendererError::BadFrameLayout),
        },
        _ => return Err(RendererError::UnsupportedFormat(fmt)),
    };
    Ok((w, gpu_h))
}

pub(crate) fn plane_texture_format(
    fmt: PixelFormat,
    plane_index: usize,
) -> Result<wgpu::TextureFormat, RendererError> {
    match fmt {
        PixelFormat::Yuv420p => {
            if plane_index > 2 {
                return Err(RendererError::BadFrameLayout);
            }
            Ok(wgpu::TextureFormat::R8Unorm)
        }
        PixelFormat::Nv12 => match plane_index {
            0 => Ok(wgpu::TextureFormat::R8Unorm),
            1 => Ok(wgpu::TextureFormat::Rg8Unorm),
            _ => Err(RendererError::BadFrameLayout),
        },
        PixelFormat::Rgba8 => {
            if plane_index != 0 {
                return Err(RendererError::BadFrameLayout);
            }
            Ok(wgpu::TextureFormat::Rgba8Unorm)
        }
        _ => Err(RendererError::UnsupportedFormat(fmt)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decoder::PixelFormat;

    #[test]
    fn yuv420p_three_planes_nv12_two_rgba_one() {
        assert_eq!(PixelFormat::Yuv420p.plane_count(), 3);
        assert_eq!(PixelFormat::Nv12.plane_count(), 2);
        assert_eq!(PixelFormat::Rgba8.plane_count(), 1);
    }

    #[test]
    fn row_bytes_yuv420p_320() {
        assert_eq!(
            row_bytes_per_plane(PixelFormat::Yuv420p, 320, 240, 0).unwrap(),
            320
        );
        assert_eq!(
            row_bytes_per_plane(PixelFormat::Yuv420p, 320, 240, 1).unwrap(),
            160
        );
    }

    #[test]
    fn row_bytes_nv12_and_rgba8() {
        assert_eq!(row_bytes_per_plane(PixelFormat::Nv12, 64, 48, 0).unwrap(), 64);
        assert_eq!(row_bytes_per_plane(PixelFormat::Nv12, 64, 48, 1).unwrap(), 64);
        assert_eq!(
            row_bytes_per_plane(PixelFormat::Rgba8, 17, 9, 0).unwrap(),
            68
        );
    }

    #[test]
    fn row_bytes_zero_frame_dimension_is_zero_dimension_error() {
        assert!(matches!(
            row_bytes_per_plane(PixelFormat::Yuv420p, 0, 240, 0),
            Err(RendererError::ZeroDimension)
        ));
        assert!(matches!(
            row_bytes_per_plane(PixelFormat::Yuv420p, 320, 0, 0),
            Err(RendererError::ZeroDimension)
        ));
    }

    #[test]
    fn row_bytes_plane_index_out_of_range_is_bad_layout() {
        assert!(matches!(
            row_bytes_per_plane(PixelFormat::Yuv420p, 320, 240, 3),
            Err(RendererError::BadFrameLayout)
        ));
        assert!(matches!(
            row_bytes_per_plane(PixelFormat::Nv12, 320, 240, 2),
            Err(RendererError::BadFrameLayout)
        ));
        assert!(matches!(
            row_bytes_per_plane(PixelFormat::Rgba8, 8, 8, 1),
            Err(RendererError::BadFrameLayout)
        ));
    }

    #[test]
    fn plane_extents_yuv420p_match_decoder_rules() {
        assert_eq!(
            plane_extent(PixelFormat::Yuv420p, 320, 240, 0).unwrap(),
            (320, 240)
        );
        assert_eq!(
            plane_extent(PixelFormat::Yuv420p, 320, 240, 1).unwrap(),
            (160, 120)
        );
        assert_eq!(
            plane_extent(PixelFormat::Yuv420p, 320, 240, 2).unwrap(),
            (160, 120)
        );
    }

    #[test]
    fn plane_extents_nv12_uv_plane_is_half_width_full_chroma_height() {
        assert_eq!(
            plane_extent(PixelFormat::Nv12, 320, 240, 1).unwrap(),
            (160, 120)
        );
    }

    #[test]
    fn plane_extent_odd_height_yuv420p_is_bad_layout() {
        assert!(matches!(
            plane_extent(PixelFormat::Yuv420p, 320, 241, 0),
            Err(RendererError::BadFrameLayout)
        ));
    }

    #[test]
    fn plane_texture_formats_match_wgpu_upload_plan() {
        assert_eq!(
            plane_texture_format(PixelFormat::Yuv420p, 1).unwrap(),
            wgpu::TextureFormat::R8Unorm
        );
        assert_eq!(
            plane_texture_format(PixelFormat::Nv12, 1).unwrap(),
            wgpu::TextureFormat::Rg8Unorm
        );
        assert_eq!(
            plane_texture_format(PixelFormat::Rgba8, 0).unwrap(),
            wgpu::TextureFormat::Rgba8Unorm
        );
    }

    #[test]
    fn plane_texture_format_bad_indices() {
        assert!(matches!(
            plane_texture_format(PixelFormat::Yuv420p, 4),
            Err(RendererError::BadFrameLayout)
        ));
        assert!(matches!(
            plane_texture_format(PixelFormat::Nv12, 2),
            Err(RendererError::BadFrameLayout)
        ));
    }
}
