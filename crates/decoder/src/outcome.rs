use crate::frame::DecodedVideoFrame;

/// Output of [`crate::Decoder::next_frame`], [`crate::Decoder::seek_scrub`], and [`crate::Decoder::seek_exact`].
///
/// **EOF is not an error:** end of file or seek past end yields `Eof`; callers clamp, show black, or advance the edit decision as appropriate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeOutcome {
    /// A full decoded frame (owned buffers).
    Frame(DecodedVideoFrame),
    /// Demuxer and codec are drained for the current read position; call a seek or reopen to continue.
    Eof,
}

#[cfg(test)]
mod tests {
    use super::DecodeOutcome;
    use crate::frame::{CpuFrame, DecodedVideoFrame, FrameData, Plane};
    use crate::{PixelFormat, Rational};

    fn tiny_frame() -> DecodedVideoFrame {
        DecodedVideoFrame {
            width: 1,
            height: 1,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 1),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Rgba8,
                planes: vec![Plane {
                    data: vec![0, 0, 0, 255],
                    stride: 4,
                }],
            }),
        }
    }

    #[test]
    fn eof_equality() {
        assert_eq!(DecodeOutcome::Eof, DecodeOutcome::Eof);
    }

    #[test]
    fn frame_variant_ne_eof() {
        let f = tiny_frame();
        assert_ne!(DecodeOutcome::Frame(f.clone()), DecodeOutcome::Eof);
    }

    #[test]
    fn frame_variant_eq_when_inner_eq() {
        let f = tiny_frame();
        assert_eq!(DecodeOutcome::Frame(f.clone()), DecodeOutcome::Frame(f));
    }

    #[test]
    fn frame_variant_ne_when_pts_differs() {
        let mut g = tiny_frame();
        g.pts = Rational::new_raw(5, 4);
        assert_ne!(
            DecodeOutcome::Frame(tiny_frame()),
            DecodeOutcome::Frame(g)
        );
    }

    #[test]
    fn outcome_clone_eof() {
        assert_eq!(DecodeOutcome::Eof.clone(), DecodeOutcome::Eof);
    }

    #[test]
    fn outcome_clone_frame_roundtrip() {
        let o = DecodeOutcome::Frame(tiny_frame());
        assert_eq!(o.clone(), o);
    }

    #[test]
    fn outcome_debug_contains_eof_tag() {
        assert!(format!("{:?}", DecodeOutcome::Eof).contains("Eof"));
    }
}
