//! Pixel formats supported for **v1 CPU** decode output.
//! Other FFmpeg pixel formats return [`crate::DecoderError::Unsupported`] at open time.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum PixelFormat {
    /// Planar YUV 4:2:0, 8-bit (`Y`, `U`, `V`).
    Yuv420p,
    /// Semi-planar 4:2:0: full `Y` plane then interleaved `UV`.
    Nv12,
    /// Packed RGBA 8 bits per channel, one plane.
    Rgba8,
}

impl PixelFormat {
    pub(crate) fn from_ffmpeg(px: ffmpeg_next::util::format::pixel::Pixel) -> Option<Self> {
        use ffmpeg_next::util::format::pixel::Pixel;
        match px {
            Pixel::YUV420P => Some(PixelFormat::Yuv420p),
            Pixel::NV12 => Some(PixelFormat::Nv12),
            Pixel::RGBA => Some(PixelFormat::Rgba8),
            _ => None,
        }
    }

    /// Number of color planes (e.g. 3 for [`PixelFormat::Yuv420p`], 2 for [`PixelFormat::Nv12`]).
    pub fn plane_count(self) -> usize {
        match self {
            PixelFormat::Yuv420p => 3,
            PixelFormat::Nv12 => 2,
            PixelFormat::Rgba8 => 1,
        }
    }

    /// Pixel height of each plane: full height for luma / RGB, half height for 4:2:0 chroma planes.
    ///
    /// Fails if `frame_height == 0` or (for 4:2:0) height is odd.
    pub fn plane_heights_full(self, frame_height: u32) -> Result<Vec<u32>, &'static str> {
        if frame_height == 0 {
            return Err("frame height is zero");
        }
        match self {
            PixelFormat::Yuv420p => {
                if !frame_height.is_multiple_of(2) {
                    return Err("YUV 4:2:0 requires even height");
                }
                Ok(vec![frame_height, frame_height / 2, frame_height / 2])
            }
            PixelFormat::Nv12 => {
                if !frame_height.is_multiple_of(2) {
                    return Err("NV12 requires even height");
                }
                Ok(vec![frame_height, frame_height / 2])
            }
            PixelFormat::Rgba8 => Ok(vec![frame_height]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PixelFormat;

    #[test]
    fn yuv420_plane_count() {
        assert_eq!(PixelFormat::Yuv420p.plane_count(), 3);
        assert_eq!(
            PixelFormat::Yuv420p.plane_heights_full(240).unwrap(),
            vec![240, 120, 120]
        );
    }

    #[test]
    fn plane_heights_rejects_zero_height() {
        assert!(PixelFormat::Yuv420p.plane_heights_full(0).is_err());
    }

    #[test]
    fn plane_heights_rejects_odd_height_for_420() {
        assert!(PixelFormat::Yuv420p.plane_heights_full(241).is_err());
    }

    #[test]
    fn nv12_two_planes() {
        assert_eq!(
            PixelFormat::Nv12.plane_heights_full(100).unwrap(),
            vec![100, 50]
        );
    }

    #[test]
    fn rgba_plane_count_one() {
        assert_eq!(PixelFormat::Rgba8.plane_count(), 1);
    }

    #[test]
    fn rgba_plane_heights_match_frame_height() {
        for h in [2u32, 4, 16, 111, 4096] {
            assert_eq!(
                PixelFormat::Rgba8.plane_heights_full(h).unwrap(),
                vec![h],
                "height {h}"
            );
        }
    }

    #[test]
    fn yuv420_plane_heights_two_through_1024() {
        for h in [2u32, 4, 8, 16, 32, 64, 128, 256, 512, 1024] {
            let got = PixelFormat::Yuv420p.plane_heights_full(h).unwrap();
            assert_eq!(got, vec![h, h / 2, h / 2]);
        }
    }

    #[test]
    fn nv12_plane_heights_two_through_512() {
        for h in [2u32, 6, 10, 22, 54, 108, 222, 512] {
            let got = PixelFormat::Nv12.plane_heights_full(h).unwrap();
            assert_eq!(got, vec![h, h / 2]);
        }
    }

    #[test]
    fn nv12_rejects_odd_height_3() {
        assert!(PixelFormat::Nv12.plane_heights_full(3).is_err());
    }

    #[test]
    fn nv12_rejects_odd_height_999() {
        assert!(PixelFormat::Nv12.plane_heights_full(999).is_err());
    }

    #[test]
    fn rgba_rejects_zero_even_though_no_subsample_rule() {
        assert!(PixelFormat::Rgba8.plane_heights_full(0).is_err());
    }

    #[test]
    fn yuv420_requires_even_at_boundary_2_ok() {
        assert!(PixelFormat::Yuv420p.plane_heights_full(2).is_ok());
    }

    #[test]
    fn all_formats_distinct_discriminants() {
        use std::mem::discriminant;
        assert_ne!(
            discriminant(&PixelFormat::Yuv420p),
            discriminant(&PixelFormat::Nv12)
        );
        assert_ne!(
            discriminant(&PixelFormat::Nv12),
            discriminant(&PixelFormat::Rgba8)
        );
    }

    #[test]
    fn pixel_format_debug_readable() {
        assert!(format!("{:?}", PixelFormat::Yuv420p).contains("Yuv420p"));
    }

    #[test]
    fn yuv420_odd_height_1023_fails() {
        assert!(PixelFormat::Yuv420p.plane_heights_full(1023).is_err());
    }
}
