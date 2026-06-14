use ffmpeg_next::util::format::pixel::Pixel;

use crate::error::DecodeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Yuv420p,
    Nv12,
    Rgba8,
}

impl PixelFormat {
    pub(crate) fn from_ffmpeg(px: Pixel) -> Option<Self> {
        match px {
            Pixel::YUV420P => Some(PixelFormat::Yuv420p),
            Pixel::NV12 => Some(PixelFormat::Nv12),
            Pixel::RGBA => Some(PixelFormat::Rgba8),
            _ => None,
        }
    }

    fn plane_heights(self, height: u32) -> Result<Vec<u32>, &'static str> {
        if height == 0 {
            return Err("frame height is zero");
        }
        match self {
            PixelFormat::Yuv420p => {
                if !height.is_multiple_of(2) {
                    return Err("YUV420P requires even height");
                }
                Ok(vec![height, height / 2, height / 2])
            }
            PixelFormat::Nv12 => {
                if !height.is_multiple_of(2) {
                    return Err("NV12 requires even height");
                }
                Ok(vec![height, height / 2])
            }
            PixelFormat::Rgba8 => Ok(vec![height]),
        }
    }

    pub fn plane_count(self) -> usize {
        match self {
            PixelFormat::Yuv420p => 3,
            PixelFormat::Nv12 => 2,
            PixelFormat::Rgba8 => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub pts_ticks: i64,
    pub format: PixelFormat,
    pub planes: Vec<Plane>,
}

impl DecodedFrame {
    pub(crate) fn from_ffmpeg(
        frame: &ffmpeg_next::util::frame::video::Video,
    ) -> Result<Self, DecodeError> {
        let width = frame.width();
        let height = frame.height();
        let format = PixelFormat::from_ffmpeg(frame.format()).ok_or_else(|| {
            DecodeError::unsupported(format!(
                "pixel format {:?} not supported after decode",
                frame.format()
            ))
        })?;
        let pts_ticks = frame
            .timestamp()
            .or_else(|| frame.pts())
            .ok_or_else(|| DecodeError::unsupported("decoded frame has no PTS"))?;

        let plane_heights = format
            .plane_heights(height)
            .map_err(DecodeError::unsupported)?;

        let mut planes = Vec::with_capacity(plane_heights.len());
        for (i, &plane_height) in plane_heights.iter().enumerate() {
            let stride = frame.stride(i);
            let need = stride
                .checked_mul(plane_height as usize)
                .ok_or_else(|| DecodeError::unsupported("plane size overflow"))?;
            let src = frame.data(i);
            if src.len() < need {
                return Err(DecodeError::unsupported(format!(
                    "plane {i}: buffer too small (have {} need {need})",
                    src.len()
                )));
            }
            planes.push(Plane {
                data: src[..need].to_vec(),
                stride,
            });
        }

        Ok(Self {
            width,
            height,
            pts_ticks,
            format,
            planes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffmpeg_next::util::format::pixel::Pixel;

    #[test]
    fn from_ffmpeg_maps_supported_pixels() {
        assert_eq!(
            PixelFormat::from_ffmpeg(Pixel::YUV420P),
            Some(PixelFormat::Yuv420p)
        );
        assert_eq!(
            PixelFormat::from_ffmpeg(Pixel::NV12),
            Some(PixelFormat::Nv12)
        );
        assert_eq!(
            PixelFormat::from_ffmpeg(Pixel::RGBA),
            Some(PixelFormat::Rgba8)
        );
    }

    #[test]
    fn from_ffmpeg_rejects_unsupported_pixels() {
        assert_eq!(PixelFormat::from_ffmpeg(Pixel::YUV422P), None);
        assert_eq!(PixelFormat::from_ffmpeg(Pixel::GRAY8), None);
    }

    #[test]
    fn plane_count_matches_format() {
        assert_eq!(PixelFormat::Yuv420p.plane_count(), 3);
        assert_eq!(PixelFormat::Nv12.plane_count(), 2);
        assert_eq!(PixelFormat::Rgba8.plane_count(), 1);
    }

    #[test]
    fn yuv_plane_heights_require_even_height() {
        assert!(PixelFormat::Yuv420p.plane_heights(1080).is_ok());
        assert_eq!(
            PixelFormat::Yuv420p.plane_heights(1080).unwrap(),
            vec![1080, 540, 540]
        );
        assert_eq!(
            PixelFormat::Yuv420p.plane_heights(1079),
            Err("YUV420P requires even height")
        );
        assert_eq!(
            PixelFormat::Nv12.plane_heights(1079),
            Err("NV12 requires even height")
        );
    }

    #[test]
    fn rgba_plane_height_matches_frame_height() {
        assert_eq!(PixelFormat::Rgba8.plane_heights(721).unwrap(), vec![721]);
    }

    #[test]
    fn zero_height_is_rejected_for_all_formats() {
        for fmt in [PixelFormat::Yuv420p, PixelFormat::Nv12, PixelFormat::Rgba8] {
            assert_eq!(fmt.plane_heights(0), Err("frame height is zero"));
        }
    }
}
