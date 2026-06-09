use std::path::{Path, PathBuf};

use crate::ids::MediaId;
use crate::time::{Rational, RationalTime, TimeRange};

/// An imported source file in the project's media pool.
///
/// This is the *asset*, not a placement on the timeline; many [`Clip`](crate::Clip)s
/// can reference the same `MediaSource`.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSource {
    pub id: MediaId,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Native frame rate of the source.
    pub frame_rate: Rational,
    /// Total length of the source at [`frame_rate`](Self::frame_rate).
    pub duration: RationalTime,
    pub has_audio: bool,
}

impl MediaSource {
    /// Create a media source with a freshly allocated [`MediaId`].
    pub fn new(
        path: impl Into<PathBuf>,
        width: u32,
        height: u32,
        frame_rate: Rational,
        duration_ticks: i64,
        has_audio: bool,
    ) -> Self {
        let duration_ticks = duration_ticks.max(0);
        Self {
            id: MediaId::next(),
            path: path.into(),
            width,
            height,
            frame_rate,
            duration: RationalTime::new(duration_ticks, frame_rate),
            has_audio,
        }
    }

    /// The full extent of the source as `[0, duration)`.
    pub fn full_range(&self) -> TimeRange {
        TimeRange::at_rate(0, self.duration.value, self.frame_rate)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const R24: Rational = Rational::FPS_24;
    const R30: Rational = Rational::FPS_30;

    fn sample(path: &str, duration: i64) -> MediaSource {
        MediaSource::new(path, 1920, 1080, R24, duration, true)
    }

    // --- MediaSource::new ---------------------------------------------------

    #[test]
    fn new_wires_all_fields() {
        let media = MediaSource::new(
            "/media/clip.mp4",
            3840,
            2160,
            R30,
            1_800,
            false,
        );

        assert_eq!(media.path, PathBuf::from("/media/clip.mp4"));
        assert_eq!(media.width, 3840);
        assert_eq!(media.height, 2160);
        assert_eq!(media.frame_rate, R30);
        assert_eq!(media.duration, RationalTime::new(1_800, R30));
        assert!(!media.has_audio);
        assert!(media.id.raw() >= 1);
    }

    #[test]
    fn new_accepts_path_buf_and_str() {
        let from_str = MediaSource::new("a.mp4", 1, 1, R24, 10, true);
        let from_buf = MediaSource::new(PathBuf::from("b.mp4"), 1, 1, R24, 10, true);
        assert_eq!(from_str.path(), Path::new("a.mp4"));
        assert_eq!(from_buf.path(), Path::new("b.mp4"));
    }

    #[test]
    fn new_assigns_distinct_media_ids() {
        let a = sample("a.mp4", 100);
        let b = sample("b.mp4", 100);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn new_clamps_negative_duration_to_zero() {
        let media = sample("short.mp4", -50);
        assert_eq!(media.duration.value, 0);
        assert_eq!(media.duration.rate, R24);
    }

    #[test]
    fn new_zero_duration_is_valid() {
        let media = sample("empty.mp4", 0);
        assert_eq!(media.duration.value, 0);
        assert!(media.full_range().is_empty());
    }

    #[test]
    fn new_duration_carries_frame_rate() {
        let ntsc = MediaSource::new(
            "ntsc.mp4",
            1920,
            1080,
            Rational::FPS_23_976,
            2_400,
            true,
        );
        assert_eq!(ntsc.frame_rate, Rational::FPS_23_976);
        assert_eq!(ntsc.duration.rate, Rational::FPS_23_976);
        assert_eq!(ntsc.duration.value, 2_400);
    }

    #[test]
    fn new_has_audio_flag() {
        assert!(sample("with-audio.mp4", 10).has_audio);
        assert!(!MediaSource::new("silent.mp4", 1, 1, R24, 10, false).has_audio);
    }

    // --- full_range -------------------------------------------------------

    #[test]
    fn full_range_spans_entire_source() {
        let media = sample("clip.mp4", 500);
        assert_eq!(
            media.full_range(),
            TimeRange::at_rate(0, 500, R24)
        );
    }

    #[test]
    fn full_range_matches_duration_at_native_rate() {
        let media = MediaSource::new("clip.mp4", 1280, 720, R30, 300, true);
        let range = media.full_range();
        assert_eq!(range.start, RationalTime::new(0, R30));
        assert_eq!(range.duration, media.duration);
        assert_eq!(range.end_tick(), 300);
    }

    #[test]
    fn full_range_zero_duration_is_empty() {
        let media = sample("blank.mp4", 0);
        let range = media.full_range();
        assert!(range.is_empty());
        assert_eq!(range.start.value, 0);
        assert_eq!(range.end_tick(), 0);
    }

    // --- path -------------------------------------------------------------

    #[test]
    fn path_returns_borrowed_path() {
        let media = sample("/vault/footage/take_01.mov", 100);
        assert_eq!(media.path(), Path::new("/vault/footage/take_01.mov"));
        assert_eq!(media.path().file_name().and_then(|s| s.to_str()), Some("take_01.mov"));
    }

    // --- Clone / Eq / Debug -----------------------------------------------

    #[test]
    fn clone_and_eq_preserve_all_fields() {
        let original = MediaSource::new("x.mp4", 640, 480, R24, 42, true);
        let cloned = original.clone();
        assert_eq!(original, cloned);
        assert_eq!(original.id, cloned.id);
        assert_eq!(original.duration, cloned.duration);
    }

    #[test]
    fn eq_requires_matching_id_and_metadata() {
        let a = MediaSource::new("same.mp4", 1920, 1080, R24, 100, true);
        let b = MediaSource::new("same.mp4", 1920, 1080, R24, 100, true);
        // Distinct `next()` ids => not equal even with identical metadata.
        assert_ne!(a, b);

        let fixed_a = MediaSource {
            id: MediaId::from_raw(1),
            path: PathBuf::from("fixed.mp4"),
            width: 1920,
            height: 1080,
            frame_rate: R24,
            duration: RationalTime::new(100, R24),
            has_audio: true,
        };
        let fixed_b = fixed_a.clone();
        assert_eq!(fixed_a, fixed_b);
    }

    #[test]
    fn debug_includes_path_and_dimensions() {
        let media = sample("debug.mp4", 10);
        let s = format!("{media:?}");
        assert!(s.contains("debug.mp4"));
        assert!(s.contains("1920"));
        assert!(s.contains("1080"));
    }
}
