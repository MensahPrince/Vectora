//! Decoder pixel formats → packed RGBA8 for `slint::Image`.
//!
//! Pure Rust on the engine thread so the UI thread never sees a YUV plane.
//! BT.709 limited-range coefficients (HD video defaults); BT.601 / full-range
//! variants can be added when we have content that needs them. Color science
//! drift on a still preview frame is barely perceptible — we'll revisit when
//! the renderer takes over and we want the GPU shader to do this anyway.

use decoder::{CpuFrame, DecodedVideoFrame, FrameData, PixelFormat, Plane};

/// Allocate an RGBA8 `width × height × 4` byte buffer from a decoded frame.
///
/// Always returns tightly packed RGBA (no row padding) regardless of the
/// source plane strides.
pub fn frame_to_rgba8(frame: &DecodedVideoFrame) -> Vec<u8> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = vec![0u8; w * h * 4];

    if let FrameData::Cpu(cpu) = &frame.data {
        match cpu.format {
            PixelFormat::Yuv420p => yuv420p_to_rgba8(cpu, w, h, &mut out),
            PixelFormat::Nv12 => nv12_to_rgba8(cpu, w, h, &mut out),
            PixelFormat::Rgba8 => rgba8_copy(cpu, w, h, &mut out),
            // `FrameData` and `PixelFormat` are both `#[non_exhaustive]`; new
            // variants (GPU surfaces, P010, …) will need their own conversion
            // paths. Until then, leave the buffer black.
            _ => {}
        }
    }

    out
}

// ---- BT.709 limited-range YUV → RGB (precomputed fixed-point coeffs) ------
//
// References: ITU-R BT.709. Limited-range Y in [16, 235], C in [16, 240].
// We work in i32 with 16.16-style scaling that fits comfortably in 32 bits
// for the value ranges involved.

#[inline(always)]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Convert one YCbCr triplet to RGB888, BT.709 limited-range.
#[inline(always)]
fn yuv_to_rgb_bt709(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    // ITU-R BT.709 limited-range:
    //   R = 1.16438356 * (Y - 16) + 1.79274107 * (V - 128)
    //   G = 1.16438356 * (Y - 16) - 0.21324861 * (U - 128) - 0.53290932 * (V - 128)
    //   B = 1.16438356 * (Y - 16) + 2.11240179 * (U - 128)
    //
    // Scaled by 1 << 16 and rounded.
    let c = (y as i32 - 16) * 76309; // 1.16438 * 65536
    let d = u as i32 - 128;
    let e = v as i32 - 128;

    let r = (c + 117504 * e + 32768) >> 16; // 1.79274 * 65536
    let g = (c - 13975 * d - 34925 * e + 32768) >> 16; // 0.21324..0.53290
    let b = (c + 138438 * d + 32768) >> 16; // 2.11240 * 65536

    (clamp_u8(r), clamp_u8(g), clamp_u8(b))
}

fn yuv420p_to_rgba8(cpu: &CpuFrame, w: usize, h: usize, out: &mut [u8]) {
    if cpu.planes.len() != 3 {
        return;
    }
    let y_plane = &cpu.planes[0];
    let u_plane = &cpu.planes[1];
    let v_plane = &cpu.planes[2];

    let y_stride = y_plane.stride;
    let u_stride = u_plane.stride;
    let v_stride = v_plane.stride;

    for row in 0..h {
        let y_row = row * y_stride;
        // 4:2:0 chroma is half-height — every 2 luma rows share one chroma row.
        let c_row = (row >> 1) * u_stride;
        let v_c_row = (row >> 1) * v_stride;
        let out_row = row * w * 4;

        for col in 0..w {
            let y = y_plane.data[y_row + col];
            let cidx = col >> 1;
            let u = u_plane.data[c_row + cidx];
            let v = v_plane.data[v_c_row + cidx];

            let (r, g, b) = yuv_to_rgb_bt709(y, u, v);
            let o = out_row + col * 4;
            out[o] = r;
            out[o + 1] = g;
            out[o + 2] = b;
            out[o + 3] = 255;
        }
    }
}

fn nv12_to_rgba8(cpu: &CpuFrame, w: usize, h: usize, out: &mut [u8]) {
    if cpu.planes.len() != 2 {
        return;
    }
    let y_plane = &cpu.planes[0];
    let uv_plane = &cpu.planes[1]; // interleaved U V U V …

    let y_stride = y_plane.stride;
    let uv_stride = uv_plane.stride;

    for row in 0..h {
        let y_row = row * y_stride;
        let c_row = (row >> 1) * uv_stride;
        let out_row = row * w * 4;

        for col in 0..w {
            let y = y_plane.data[y_row + col];
            let cidx = (col >> 1) * 2;
            let u = uv_plane.data[c_row + cidx];
            let v = uv_plane.data[c_row + cidx + 1];

            let (r, g, b) = yuv_to_rgb_bt709(y, u, v);
            let o = out_row + col * 4;
            out[o] = r;
            out[o + 1] = g;
            out[o + 2] = b;
            out[o + 3] = 255;
        }
    }
}

fn rgba8_copy(cpu: &CpuFrame, w: usize, h: usize, out: &mut [u8]) {
    let Some(Plane { data, stride }) = cpu.planes.first() else {
        return;
    };
    let row_bytes = w * 4;
    for row in 0..h {
        let src = row * *stride;
        let dst = row * row_bytes;
        out[dst..dst + row_bytes].copy_from_slice(&data[src..src + row_bytes]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decoder::{CpuFrame, DecodedVideoFrame, FrameData, PixelFormat, Plane, Rational};

    fn rgba_frame(w: u32, h: u32, fill: [u8; 4]) -> DecodedVideoFrame {
        let row_bytes = (w * 4) as usize;
        let stride = row_bytes + 16; // pretend the decoder padded rows
        let mut data = vec![0u8; stride * h as usize];
        for row in 0..h as usize {
            let off = row * stride;
            for col in 0..w as usize {
                data[off + col * 4..off + col * 4 + 4].copy_from_slice(&fill);
            }
        }
        DecodedVideoFrame {
            width: w,
            height: h,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 30),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Rgba8,
                planes: vec![Plane { data, stride }],
            }),
        }
    }

    #[test]
    fn rgba_passthrough_strips_padding() {
        let f = rgba_frame(2, 2, [10, 20, 30, 255]);
        let out = frame_to_rgba8(&f);
        assert_eq!(out.len(), 2 * 2 * 4);
        for px in out.chunks_exact(4) {
            assert_eq!(px, &[10, 20, 30, 255]);
        }
    }

    #[test]
    fn yuv420_grey_renders_grey() {
        // Y = 126 (limited-range mid-grey), U = V = 128 → R = G = B ≈ 128.
        let w = 4usize;
        let h = 4usize;
        let y = vec![126u8; w * h];
        let u = vec![128u8; (w / 2) * (h / 2)];
        let v = vec![128u8; (w / 2) * (h / 2)];

        let frame = DecodedVideoFrame {
            width: w as u32,
            height: h as u32,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 30),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Yuv420p,
                planes: vec![
                    Plane { data: y, stride: w },
                    Plane {
                        data: u,
                        stride: w / 2,
                    },
                    Plane {
                        data: v,
                        stride: w / 2,
                    },
                ],
            }),
        };

        let out = frame_to_rgba8(&frame);
        for px in out.chunks_exact(4) {
            // R, G, B within a couple LSBs of each other and near mid-grey.
            assert!((px[0] as i32 - 128).abs() < 8);
            assert_eq!(px[0], px[1]);
            assert_eq!(px[1], px[2]);
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn nv12_grey_renders_grey() {
        let w = 4usize;
        let h = 4usize;
        let y = vec![126u8; w * h];
        let mut uv = vec![0u8; w * (h / 2)]; // interleaved per row
        for chunk in uv.chunks_exact_mut(2) {
            chunk[0] = 128;
            chunk[1] = 128;
        }

        let frame = DecodedVideoFrame {
            width: w as u32,
            height: h as u32,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 30),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Nv12,
                planes: vec![
                    Plane { data: y, stride: w },
                    Plane { data: uv, stride: w },
                ],
            }),
        };

        let out = frame_to_rgba8(&frame);
        for px in out.chunks_exact(4) {
            assert!((px[0] as i32 - 128).abs() < 8);
            assert_eq!(px[0], px[1]);
            assert_eq!(px[1], px[2]);
            assert_eq!(px[3], 255);
        }
    }
}
