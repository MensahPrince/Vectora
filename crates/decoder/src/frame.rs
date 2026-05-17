use crate::{pixel::PixelFormat, time::Rational};

/// One color plane: **owned** byte buffer and **stride** (bytes per row, ≥ effective row width).
///
/// FFmpeg often pads rows to 16/32-byte boundaries; consumers must use `stride`, not `width × bpp`, for indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane {
    pub data: Vec<u8>,
    /// Bytes per scanline including padding.
    pub stride: usize,
}

/// CPU memory copy of a decoded picture. A future [`FrameData`] variant may hold GPU surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuFrame {
    pub format: PixelFormat,
    pub planes: Vec<Plane>,
}

/// Decoded picture memory. **Non-exhaustive:** GPU / platform variants may be added without a major version strategy change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameData {
    /// Host memory (`Vec<u8>` per plane).
    Cpu(CpuFrame),
}

/// One decoded video frame: dimensions, **presentation** time in seconds, and owned pixels.
///
/// `pts` uses [`Rational`] (seconds). It is derived from FFmpeg `best_effort_timestamp` when present, else `pts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedVideoFrame {
    pub width: u32,
    pub height: u32,
    /// Presentation time in **seconds** along the media timeline (`stream_ticks × time_base`).
    pub pts: Rational,
    /// Stream timebase used to interpret source timestamps (`num`/`den` seconds per tick style; matches probe [`crate::SourceInfo::timebase`]).
    pub timebase: Rational,
    pub data: FrameData,
}

#[cfg(test)]
mod tests {
    use super::{CpuFrame, DecodedVideoFrame, FrameData, Plane};
    use crate::{PixelFormat, Rational};

    fn rgba_tile_frame() -> DecodedVideoFrame {
        DecodedVideoFrame {
            width: 2,
            height: 2,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 30_000),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Rgba8,
                planes: vec![Plane {
                    data: vec![7u8; 16],
                    stride: 8,
                }],
            }),
        }
    }

    #[test]
    fn plane_eq_same_stride_and_bytes() {
        let a = Plane {
            data: vec![1, 2, 3],
            stride: 3,
        };
        let b = Plane {
            data: vec![1, 2, 3],
            stride: 3,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn plane_ne_different_stride() {
        let a = Plane {
            data: vec![0; 4],
            stride: 2,
        };
        let b = Plane {
            data: vec![0; 4],
            stride: 4,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn plane_ne_different_bytes() {
        let a = Plane {
            data: vec![1],
            stride: 1,
        };
        let b = Plane {
            data: vec![2],
            stride: 1,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn cpu_frame_eq_matches_format_and_planes() {
        let f = PixelFormat::Nv12;
        let p = vec![
            Plane {
                data: vec![9; 4],
                stride: 2,
            },
            Plane {
                data: vec![8; 4],
                stride: 2,
            },
        ];
        assert_eq!(
            CpuFrame {
                format: f,
                planes: p.clone(),
            },
            CpuFrame {
                format: f,
                planes: p,
            }
        );
    }

    #[test]
    fn cpu_frame_ne_on_format() {
        let mk = |fmt| CpuFrame {
            format: fmt,
            planes: vec![Plane {
                data: vec![0],
                stride: 1,
            }],
        };
        assert_ne!(mk(PixelFormat::Rgba8), mk(PixelFormat::Yuv420p));
    }

    #[test]
    fn frame_data_cpu_wrap_eq() {
        let cpu = CpuFrame {
            format: PixelFormat::Yuv420p,
            planes: vec![
                Plane {
                    data: vec![0],
                    stride: 1,
                },
                Plane {
                    data: vec![0],
                    stride: 1,
                },
                Plane {
                    data: vec![0],
                    stride: 1,
                },
            ],
        };
        assert_eq!(FrameData::Cpu(cpu.clone()), FrameData::Cpu(cpu));
    }

    #[test]
    fn decoded_video_frame_eq_all_fields() {
        let a = rgba_tile_frame();
        let b = rgba_tile_frame();
        assert_eq!(a, b);
    }

    #[test]
    fn decoded_video_frame_ne_on_pts() {
        let mut b = rgba_tile_frame();
        b.pts = Rational::new_raw(1, 1);
        assert_ne!(rgba_tile_frame(), b);
    }

    #[test]
    fn decoded_video_frame_ne_on_timebase() {
        let mut b = rgba_tile_frame();
        b.timebase = Rational::new_raw(1, 60_000);
        assert_ne!(rgba_tile_frame(), b);
    }

    #[test]
    fn decoded_video_frame_ne_on_dimensions() {
        let mut b = rgba_tile_frame();
        b.width = 3;
        assert_ne!(rgba_tile_frame(), b);
    }

    #[test]
    fn decoded_clone_preserves_eq() {
        let a = rgba_tile_frame();
        let c = a.clone();
        assert_eq!(a, c);
    }

    #[test]
    fn plane_debug_is_bounded() {
        let p = Plane {
            data: vec![0xff; 4],
            stride: 2,
        };
        let s = format!("{p:?}");
        assert!(s.contains("stride"));
        assert!(s.len() < 400);
    }

    #[test]
    fn frame_data_debug_non_exhaustive_cpu_only() {
        let d = FrameData::Cpu(CpuFrame {
            format: PixelFormat::Rgba8,
            planes: vec![],
        });
        assert!(format!("{d:?}").contains("Cpu"));
    }
}
