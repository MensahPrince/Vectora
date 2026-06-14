//! Frame buffers for preview and export.
//!
//! GPU conversion is the default path ([`decoded_to_yuv_layer`]). CPU routines
//! in this module are legacy fallbacks kept for tests and [`ColorConvertPath::LegacyCpu`].

use cutlass_compositor::Yuv420pLayer;
use cutlass_decoder::{DecodedFrame, PixelFormat};

use crate::error::EngineError;

/// RGBA8 preview frame (row-major, tightly packed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

impl RgbaFrame {
    pub fn new(width: u32, height: u32, bytes: Vec<u8>) -> Result<Self, EngineError> {
        let expected = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().map(|h| w * h * 4))
            .ok_or_else(|| EngineError::Preview("invalid frame dimensions".into()))?;
        if bytes.len() != expected {
            return Err(EngineError::Preview(format!(
                "rgba buffer is {} bytes, expected {expected}",
                bytes.len()
            )));
        }
        Ok(Self {
            width,
            height,
            bytes,
        })
    }
}

/// Build a [`Yuv420pLayer`] from a decoded frame for GPU conversion.
pub fn decoded_to_yuv_layer(frame: &DecodedFrame) -> Result<Yuv420pLayer, EngineError> {
    match frame.format {
        PixelFormat::Yuv420p => {
            let y = &frame.planes[0];
            let u = &frame.planes[1];
            let v = &frame.planes[2];
            Ok(Yuv420pLayer::new(
                frame.width,
                frame.height,
                y.data.clone(),
                y.stride as u32,
                u.data.clone(),
                u.stride as u32,
                v.data.clone(),
                v.stride as u32,
            ))
        }
        PixelFormat::Rgba8 => Err(EngineError::Preview(
            "RGBA source must use legacy CPU path".into(),
        )),
        PixelFormat::Nv12 => Err(EngineError::Preview(
            "NV12 preview conversion not implemented yet".into(),
        )),
    }
}

/// Legacy CPU YUV/RGBA conversion.
pub fn legacy_decoded_to_rgba(frame: &DecodedFrame) -> Result<RgbaFrame, EngineError> {
    decoded_to_rgba_inner(frame)
}

/// Legacy alias used by older tests and [`ColorConvertPath::LegacyCpu`].
#[allow(dead_code)]
pub fn decoded_to_rgba(frame: &DecodedFrame) -> Result<RgbaFrame, EngineError> {
    legacy_decoded_to_rgba(frame)
}

fn decoded_to_rgba_inner(frame: &DecodedFrame) -> Result<RgbaFrame, EngineError> {
    match frame.format {
        PixelFormat::Rgba8 => rgba_from_rgba8(frame),
        PixelFormat::Yuv420p => yuv420p_to_rgba(frame),
        PixelFormat::Nv12 => Err(EngineError::Preview(
            "NV12 preview conversion not implemented yet".into(),
        )),
    }
}

fn rgba_from_rgba8(frame: &DecodedFrame) -> Result<RgbaFrame, EngineError> {
    let plane = frame
        .planes
        .first()
        .ok_or_else(|| EngineError::Preview("RGBA frame has no plane".into()))?;
    let w = frame.width as usize;
    let h = frame.height as usize;
    let mut bytes = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let start = row * plane.stride;
        let end = start + w * 4;
        if end > plane.data.len() {
            return Err(EngineError::Preview("RGBA plane row out of bounds".into()));
        }
        bytes.extend_from_slice(&plane.data[start..end]);
    }
    RgbaFrame::new(frame.width, frame.height, bytes)
}

/// Tight-packed YUV420P (Y, then U, then V) for the frame cache.
pub fn pack_yuv420p(frame: &DecodedFrame) -> Result<Vec<u8>, EngineError> {
    if frame.format != PixelFormat::Yuv420p {
        return Err(EngineError::Preview(
            "expected YUV420P for cache pack".into(),
        ));
    }
    let w = frame.width as usize;
    let h = frame.height as usize;
    if w == 0 || h == 0 || !h.is_multiple_of(2) || !w.is_multiple_of(2) {
        return Err(EngineError::Preview("invalid YUV420P dimensions".into()));
    }
    let y = &frame.planes[0];
    let u = &frame.planes[1];
    let v = &frame.planes[2];
    let mut out = Vec::with_capacity(w * h + (w / 2) * (h / 2) * 2);
    for row in 0..h {
        let start = row * y.stride;
        out.extend_from_slice(&y.data[start..start + w]);
    }
    let uv_h = h / 2;
    let uv_w = w / 2;
    for row in 0..uv_h {
        let start = row * u.stride;
        out.extend_from_slice(&u.data[start..start + uv_w]);
    }
    for row in 0..uv_h {
        let start = row * v.stride;
        out.extend_from_slice(&v.data[start..start + uv_w]);
    }
    Ok(out)
}

pub fn unpack_yuv420p(bytes: &[u8], width: u32, height: u32) -> Result<DecodedFrame, EngineError> {
    let w = width as usize;
    let h = height as usize;
    let y_size = w * h;
    let uv_size = (w / 2) * (h / 2);
    let need = y_size + uv_size * 2;
    if bytes.len() != need {
        return Err(EngineError::Preview(format!(
            "packed YUV is {} bytes, expected {need}",
            bytes.len()
        )));
    }
    Ok(DecodedFrame {
        width,
        height,
        pts_ticks: 0,
        format: PixelFormat::Yuv420p,
        planes: vec![
            cutlass_decoder::Plane {
                data: bytes[..y_size].to_vec(),
                stride: w,
            },
            cutlass_decoder::Plane {
                data: bytes[y_size..y_size + uv_size].to_vec(),
                stride: w / 2,
            },
            cutlass_decoder::Plane {
                data: bytes[y_size + uv_size..].to_vec(),
                stride: w / 2,
            },
        ],
    })
}

fn yuv420p_to_rgba(frame: &DecodedFrame) -> Result<RgbaFrame, EngineError> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let y_plane = &frame.planes[0];
    let u_plane = &frame.planes[1];
    let v_plane = &frame.planes[2];
    let mut rgba = vec![0u8; w * h * 4];

    for row in 0..h {
        for col in 0..w {
            let y = i32::from(y_plane.data[row * y_plane.stride + col]);
            let uv_row = row / 2;
            let uv_col = col / 2;
            let u = i32::from(u_plane.data[uv_row * u_plane.stride + uv_col]) - 128;
            let v = i32::from(v_plane.data[uv_row * v_plane.stride + uv_col]) - 128;

            let r = ((298 * (y - 16) + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * (y - 16) - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * (y - 16) + 516 * u + 128) >> 8).clamp(0, 255) as u8;

            let i = (row * w + col) * 4;
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 255;
        }
    }

    RgbaFrame::new(frame.width, frame.height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutlass_decoder::Plane;

    fn solid_yuv420p(width: u32, height: u32, y: u8, u: u8, v: u8) -> DecodedFrame {
        let w = width as usize;
        let h = height as usize;
        DecodedFrame {
            width,
            height,
            pts_ticks: 0,
            format: PixelFormat::Yuv420p,
            planes: vec![
                Plane {
                    data: vec![y; w * h],
                    stride: w,
                },
                Plane {
                    data: vec![u; (w / 2) * (h / 2)],
                    stride: w / 2,
                },
                Plane {
                    data: vec![v; (w / 2) * (h / 2)],
                    stride: w / 2,
                },
            ],
        }
    }

    #[test]
    fn pack_unpack_roundtrip() {
        let frame = solid_yuv420p(64, 64, 128, 128, 128);
        let packed = pack_yuv420p(&frame).unwrap();
        let restored = unpack_yuv420p(&packed, 64, 64).unwrap();
        assert_eq!(restored.planes[0].data, frame.planes[0].data);
        assert_eq!(restored.planes[1].data, frame.planes[1].data);
    }

    #[test]
    fn yuv_gray_maps_to_neutral_rgb() {
        let frame = solid_yuv420p(2, 2, 128, 128, 128);
        let rgba = yuv420p_to_rgba(&frame).unwrap();
        assert_eq!(rgba.bytes.len(), 16);
        assert_eq!(rgba.bytes[0], rgba.bytes[1]);
        assert_eq!(rgba.bytes[1], rgba.bytes[2]);
        assert_eq!(rgba.bytes[3], 255);
    }
}
