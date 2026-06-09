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
