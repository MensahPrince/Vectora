//! Shared seek policy: roll forward instead of re-seeking for near targets.
//!
//! None of the native readers keeps a keyframe index (the old FFmpeg stack
//! did), so the [`VideoDecoder`] default `frame_at` — seek, then walk — makes
//! the codec re-decode the whole GOP prefix on *every* call. Across a playback
//! or forward-scrub run that is O(GOP²) work per GOP, and scrubbing is the
//! primary touch interaction on mobile.
//!
//! Every backend therefore remembers the PTS of the last frame it emitted and
//! answers `frame_at` with [`frame_at_rolling`]: when the target lies ahead of
//! that frame by at most [`ROLL_FORWARD_WINDOW_SECS`], keep decoding forward
//! from where the codec already is — every in-between frame would have to be
//! decoded after a seek anyway. Backward targets, long forward jumps, and
//! fresh decoders fall back to a real seek, byte-identical to the default
//! seek-then-walk.
//!
//! The window bounds the worst case (rolling past an unnoticed keyframe can't
//! waste more than a window of decode) while comfortably covering the
//! sequential case (gaps of one frame). One second ≈ a typical mobile-capture
//! GOP.

use core::cmp::Ordering;

use cutlass_core::{DecodeError, RationalTime, VideoDecoder, VideoFrame};

/// How far ahead of the last emitted frame a target may lie and still be
/// reached by decoding forward rather than seeking, in seconds.
const ROLL_FORWARD_WINDOW_SECS: i64 = 1;

/// True when `target` is strictly after `last_pts` by no more than the roll
/// window, i.e. the decoder should keep pulling frames instead of seeking.
///
/// Exact cross-rate arithmetic in `i128` (values ≤ 2⁶³, rate parts ≤ 2³¹, so
/// the triple products stay below 2¹²⁵): with `sec(t) = value·den/num`,
/// `sec(target) − sec(last) ≤ W` cross-multiplied by both (positive)
/// numerators becomes the comparison below. Invalid rates disable rolling —
/// a seek is always correct.
pub(crate) fn should_roll_forward(last_pts: Option<RationalTime>, target: RationalTime) -> bool {
    let Some(last) = last_pts else {
        return false;
    };
    if !last.rate.is_valid() || !target.rate.is_valid() {
        return false;
    }
    if target.compare(last) != Ordering::Greater {
        return false;
    }
    let ahead = i128::from(target.value) * i128::from(target.rate.den) * i128::from(last.rate.num)
        - i128::from(last.value) * i128::from(last.rate.den) * i128::from(target.rate.num);
    let window = i128::from(ROLL_FORWARD_WINDOW_SECS)
        * i128::from(target.rate.num)
        * i128::from(last.rate.num);
    ahead <= window
}

/// [`VideoDecoder::frame_at`] with the roll-forward fast path.
///
/// `last_pts` is the PTS of the last frame the decoder emitted (backends
/// record it in `next_frame` and clear it in `seek`). When rolling, the walk
/// continues from the decoder's current position; hitting end of stream while
/// rolling means the target lies past the end, which is the same `Ok(None)`
/// the seek path would produce.
pub(crate) fn frame_at_rolling<D: VideoDecoder + ?Sized>(
    decoder: &mut D,
    last_pts: Option<RationalTime>,
    target: RationalTime,
) -> Result<Option<VideoFrame>, DecodeError> {
    if !should_roll_forward(last_pts, target) {
        decoder.seek(target)?;
    }
    while let Some(frame) = decoder.next_frame()? {
        if frame.pts.compare(target) != Ordering::Less {
            return Ok(Some(frame));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutlass_core::{
        ColorSpace, CpuImage, FrameData, PixelFormat, Plane, Rational, Rect, Rotation, SourceInfo,
    };

    const R30: Rational = Rational::FPS_30;

    fn rt(value: i64, rate: Rational) -> RationalTime {
        RationalTime::new(value, rate)
    }

    #[test]
    fn no_last_pts_never_rolls() {
        assert!(!should_roll_forward(None, rt(5, R30)));
    }

    #[test]
    fn backward_and_repeated_targets_never_roll() {
        let last = Some(rt(10, R30));
        assert!(!should_roll_forward(last, rt(9, R30)));
        assert!(!should_roll_forward(last, rt(10, R30)));
    }

    #[test]
    fn rolls_within_the_window() {
        let last = Some(rt(10, R30));
        // One frame ahead — the sequential-playback case.
        assert!(should_roll_forward(last, rt(11, R30)));
        // Exactly one second ahead is still inside the (inclusive) window.
        assert!(should_roll_forward(last, rt(40, R30)));
        // Just past the window falls back to a seek.
        assert!(!should_roll_forward(last, rt(41, R30)));
    }

    #[test]
    fn window_comparison_is_exact_across_rates() {
        // Last frame on an NTSC stream time base, target at plain 30 fps.
        let last = Some(rt(0, Rational::FPS_29_97));
        // 29/30 s ahead: inside the window.
        assert!(should_roll_forward(last, rt(29, R30)));
        // 31/30 s ahead: outside.
        assert!(!should_roll_forward(last, rt(31, R30)));
        // Stream-tick time bases (large numerators) stay exact too.
        let last_ticks = Some(rt(90_000, Rational::new(90_000, 1))); // t = 1 s
        assert!(should_roll_forward(last_ticks, rt(60, R30))); // 2 s
        assert!(!should_roll_forward(last_ticks, rt(61, R30))); // 2 s + 1 frame
    }

    #[test]
    fn invalid_rates_never_roll() {
        let last = Some(rt(10, Rational::new(0, 1)));
        assert!(!should_roll_forward(last, rt(11, R30)));
        assert!(!should_roll_forward(
            Some(rt(10, R30)),
            rt(11, Rational::new(30, 0))
        ));
    }

    /// A decoder over `count` synthetic 30 fps frames with a keyframe only at
    /// zero, mirroring how the real backends wire [`frame_at_rolling`]:
    /// `next_frame` records `last_pts`, `seek` clears it and counts.
    struct MockDecoder {
        info: SourceInfo,
        cursor: i64,
        count: i64,
        last_pts: Option<RationalTime>,
        seeks: usize,
    }

    impl MockDecoder {
        fn new(count: i64) -> Self {
            Self {
                info: SourceInfo {
                    coded_size: (2, 2),
                    display_size: (2, 2),
                    rotation: Rotation::None,
                    pixel_format: PixelFormat::Nv12,
                    color: ColorSpace::BT709,
                    frame_rate: R30,
                    time_base: R30,
                    duration: Some(rt(count, R30)),
                },
                cursor: 0,
                count,
                last_pts: None,
                seeks: 0,
            }
        }
    }

    impl VideoDecoder for MockDecoder {
        fn info(&self) -> &SourceInfo {
            &self.info
        }

        fn seek(&mut self, _target: RationalTime) -> Result<(), DecodeError> {
            self.cursor = 0; // only keyframe is frame 0
            self.last_pts = None;
            self.seeks += 1;
            Ok(())
        }

        fn next_frame(&mut self) -> Result<Option<VideoFrame>, DecodeError> {
            if self.cursor >= self.count {
                return Ok(None);
            }
            let pts = rt(self.cursor, R30);
            self.cursor += 1;
            self.last_pts = Some(pts);
            let y = Plane::new(vec![16u8; 4], 2, 2);
            let uv = Plane::new(vec![128u8; 2], 2, 1);
            Ok(Some(VideoFrame::new(
                pts,
                PixelFormat::Nv12,
                ColorSpace::BT709,
                (2, 2),
                Rect::from_size(2, 2),
                Rotation::None,
                FrameData::Cpu(CpuImage::new(vec![y, uv])),
            )))
        }

        fn frame_at(&mut self, target: RationalTime) -> Result<Option<VideoFrame>, DecodeError> {
            let last = self.last_pts;
            frame_at_rolling(self, last, target)
        }
    }

    #[test]
    fn sequential_targets_seek_once_and_land_exactly() {
        let mut dec = MockDecoder::new(60);
        for i in (0..30).step_by(3) {
            let f = dec.frame_at(rt(i, R30)).unwrap().unwrap();
            assert_eq!(f.pts.value, i);
        }
        assert_eq!(dec.seeks, 1, "only the first (cold) target may seek");
    }

    #[test]
    fn backward_target_falls_back_to_seek() {
        let mut dec = MockDecoder::new(60);
        dec.frame_at(rt(20, R30)).unwrap().unwrap();
        let f = dec.frame_at(rt(5, R30)).unwrap().unwrap();
        assert_eq!(f.pts.value, 5);
        assert_eq!(dec.seeks, 2);
    }

    #[test]
    fn far_forward_target_falls_back_to_seek() {
        let mut dec = MockDecoder::new(120);
        dec.frame_at(rt(0, R30)).unwrap().unwrap();
        // 40/30 s ahead of frame 0: beyond the window.
        let f = dec.frame_at(rt(40, R30)).unwrap().unwrap();
        assert_eq!(f.pts.value, 40);
        assert_eq!(dec.seeks, 2);
    }

    #[test]
    fn rolling_past_end_of_stream_returns_none() {
        let mut dec = MockDecoder::new(10);
        dec.frame_at(rt(9, R30)).unwrap().unwrap();
        // Within the roll window but past the last frame.
        assert!(dec.frame_at(rt(12, R30)).unwrap().is_none());
        assert_eq!(dec.seeks, 1, "the roll path must not seek");
    }
}
