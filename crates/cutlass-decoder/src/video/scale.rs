//! CPU YUV420P downscale (libswscale) for the preview frame cache.
//!
//! The preview pipeline caches and composites at a reduced resolution while
//! export keeps the full source. Decode still runs at native resolution
//! (the codec has no cheaper option), then a freshly decoded frame is scaled
//! down once, before it enters the cache — so every later cache hit, GPU
//! upload, composite, and readback is the smaller size.

use ffmpeg_next::format::Pixel;
use ffmpeg_next::software::scaling;
use ffmpeg_next::util::frame::video::Video;

use crate::error::DecodeError;
use crate::video::frame::{DecodedFrame, PixelFormat, Plane};

/// Downscale a YUV420P [`DecodedFrame`] to `dst_w × dst_h` (both must be even).
///
/// Uses `AREA` resampling — the right filter for downscaling (box average),
/// matching the thumbnail/strip path. The PTS is preserved so cache keying is
/// unchanged. Returns the input unchanged when it already matches the target.
pub fn scale_yuv420p(
    frame: &DecodedFrame,
    dst_w: u32,
    dst_h: u32,
) -> Result<DecodedFrame, DecodeError> {
    if frame.format != PixelFormat::Yuv420p {
        return Err(DecodeError::unsupported("scale_yuv420p requires YUV420P input"));
    }
    if frame.width == dst_w && frame.height == dst_h {
        return Ok(frame.clone());
    }
    if dst_w == 0 || dst_h == 0 || !dst_w.is_multiple_of(2) || !dst_h.is_multiple_of(2) {
        return Err(DecodeError::unsupported(
            "scale_yuv420p target must be non-zero and even",
        ));
    }
    if frame.planes.len() < 3 {
        return Err(DecodeError::unsupported("YUV420P frame missing planes"));
    }

    // Rebuild an ffmpeg frame from our packed-stride planes so swscale can read it.
    let mut src = Video::new(Pixel::YUV420P, frame.width, frame.height);
    for (i, plane) in frame.planes.iter().enumerate().take(3) {
        let plane_w = if i == 0 {
            frame.width as usize
        } else {
            (frame.width / 2) as usize
        };
        let plane_h = if i == 0 {
            frame.height as usize
        } else {
            (frame.height / 2) as usize
        };
        let dst_stride = src.stride(i);
        let dst = src.data_mut(i);
        for row in 0..plane_h {
            let s = row * plane.stride;
            let d = row * dst_stride;
            dst[d..d + plane_w].copy_from_slice(&plane.data[s..s + plane_w]);
        }
    }

    let mut scaler = scaling::Context::get(
        Pixel::YUV420P,
        frame.width,
        frame.height,
        Pixel::YUV420P,
        dst_w,
        dst_h,
        scaling::Flags::AREA,
    )
    .map_err(DecodeError::Decode)?;

    let mut out = Video::empty();
    scaler.run(&src, &mut out).map_err(DecodeError::Decode)?;

    let mut planes = Vec::with_capacity(3);
    for i in 0..3 {
        let plane_h = if i == 0 {
            dst_h as usize
        } else {
            (dst_h / 2) as usize
        };
        let stride = out.stride(i);
        planes.push(Plane {
            data: out.data(i)[..stride * plane_h].to_vec(),
            stride,
        });
    }

    Ok(DecodedFrame {
        width: dst_w,
        height: dst_h,
        pts_ticks: frame.pts_ticks,
        format: PixelFormat::Yuv420p,
        planes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, y: u8, u: u8, v: u8) -> DecodedFrame {
        let w = width as usize;
        let h = height as usize;
        DecodedFrame {
            width,
            height,
            pts_ticks: 42,
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

    /// Every valid pixel (ignoring swscale's row-stride padding) equals `val`.
    fn plane_is_solid(plane: &Plane, width: usize, height: usize, val: u8) -> bool {
        (0..height).all(|row| {
            let start = row * plane.stride;
            plane.data[start..start + width].iter().all(|&p| p == val)
        })
    }

    #[test]
    fn downscale_halves_dimensions_and_keeps_pts() {
        let src = solid(64, 64, 120, 130, 140);
        let out = scale_yuv420p(&src, 32, 32).expect("scale");
        assert_eq!((out.width, out.height), (32, 32));
        assert_eq!(out.pts_ticks, 42);
        assert_eq!(out.format, PixelFormat::Yuv420p);
        // A solid frame stays solid through AREA resampling (valid pixels only;
        // swscale pads each row's stride past the active width).
        assert!(plane_is_solid(&out.planes[0], 32, 32, 120));
        assert!(plane_is_solid(&out.planes[1], 16, 16, 130));
        assert!(plane_is_solid(&out.planes[2], 16, 16, 140));
    }

    #[test]
    fn same_size_is_identity() {
        let src = solid(16, 16, 1, 2, 3);
        let out = scale_yuv420p(&src, 16, 16).expect("scale");
        assert_eq!(out, src);
    }

    #[test]
    fn odd_target_is_rejected() {
        let src = solid(16, 16, 1, 2, 3);
        assert!(scale_yuv420p(&src, 15, 16).is_err());
    }
}
