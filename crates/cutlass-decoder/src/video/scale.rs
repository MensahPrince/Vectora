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
        return Err(DecodeError::unsupported(
            "scale_yuv420p requires YUV420P input",
        ));
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
        // Downscaling is range-agnostic — the studio/full swing of the input
        // samples carries straight through.
        full_range: frame.full_range,
    })
}

/// Pixel formats the rest of the pipeline ingests directly, no conversion
/// needed: limited-range planar YUV420P, full-range `yuvj420p` (same layout,
/// range tracked separately), semi-planar NV12 (the usual hardware transfer
/// output), and packed RGBA.
pub(crate) fn is_pipeline_pixel_format(px: Pixel) -> bool {
    matches!(
        px,
        Pixel::YUV420P | Pixel::YUVJ420P | Pixel::NV12 | Pixel::RGBA
    )
}

/// Convert a decoded frame in any other pixel format to limited-range
/// [`Pixel::YUV420P`] via libswscale, preserving dimensions.
///
/// Real-world media is full of formats the engine can't composite directly —
/// full-range `yuvj420p` (phone clips, screen recordings), 10-bit
/// (`yuv420p10le` / `p010le`), and 4:2:2 / 4:4:4 chroma. Rather than rejecting
/// them at open/decode time, normalize once here so preview and export work.
/// swscale applies the color-range (full → limited) and chroma-subsampling
/// conversion; everything downstream keeps its limited-range YUV420P
/// assumption (see `yuv_blit.wgsl`).
pub(crate) fn convert_to_yuv420p(src: &Video) -> Result<Video, DecodeError> {
    let mut scaler = scaling::Context::get(
        src.format(),
        src.width(),
        src.height(),
        Pixel::YUV420P,
        src.width(),
        src.height(),
        scaling::Flags::BILINEAR,
    )
    .map_err(DecodeError::Decode)?;

    let mut out = Video::empty();
    scaler.run(src, &mut out).map_err(DecodeError::Decode)?;
    // swscale does not propagate timing; carry the source PTS over so the
    // frame still keys correctly into the cache and timeline.
    out.set_pts(src.pts().or_else(|| src.timestamp()));
    Ok(out)
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
            full_range: false,
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

    #[test]
    fn pipeline_formats_skip_conversion() {
        assert!(is_pipeline_pixel_format(Pixel::YUV420P));
        assert!(is_pipeline_pixel_format(Pixel::NV12));
        assert!(is_pipeline_pixel_format(Pixel::RGBA));
        // yuvj420p shares the YUV420P layout, so it rides the fast path too —
        // its full range is handled downstream on the GPU, not by swscale.
        assert!(is_pipeline_pixel_format(Pixel::YUVJ420P));
        // Genuinely different layouts still need normalization.
        assert!(!is_pipeline_pixel_format(Pixel::YUV420P10LE));
        assert!(!is_pipeline_pixel_format(Pixel::YUV422P));
        assert!(!is_pipeline_pixel_format(Pixel::YUVJ422P));
    }

    fn fill_plane(frame: &mut Video, plane: usize, val: u8) {
        for b in frame.data_mut(plane).iter_mut() {
            *b = val;
        }
    }

    /// Exotic full-range chroma layouts (here `yuvj422p`) can't be relabeled —
    /// swscale must both subsample chroma to 4:2:0 and remap the 0..255 luma
    /// into the 16..235 limited swing the GPU's default matrix expects.
    #[test]
    fn full_range_422_normalizes_to_limited_range_yuv420p() {
        let (w, h) = (16u32, 16u32);

        let mut white = Video::new(Pixel::YUVJ422P, w, h);
        fill_plane(&mut white, 0, 255);
        fill_plane(&mut white, 1, 128);
        fill_plane(&mut white, 2, 128);
        let out = convert_to_yuv420p(&white).expect("convert white");
        assert_eq!(out.format(), Pixel::YUV420P);
        assert_eq!((out.width(), out.height()), (w, h));
        let y_white = out.data(0)[0];
        assert!(
            (230..=240).contains(&y_white),
            "full-range white (255) should compress to limited white (~235), got {y_white}"
        );

        let mut black = Video::new(Pixel::YUVJ422P, w, h);
        fill_plane(&mut black, 0, 0);
        fill_plane(&mut black, 1, 128);
        fill_plane(&mut black, 2, 128);
        let out = convert_to_yuv420p(&black).expect("convert black");
        let y_black = out.data(0)[0];
        assert!(
            (12..=20).contains(&y_black),
            "full-range black (0) should lift to limited black (~16), got {y_black}"
        );
    }
}
