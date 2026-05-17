use crate::{pixel::PixelFormat, time::Rational};

/// Snapshot from the demuxer / decoder after [`crate::Decoder::open`]: geometry, timebase, optional duration, negotiated pixel format.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceInfo {
    /// Decoded picture width in pixels.
    pub width: u32,
    /// Decoded picture height in pixels.
    pub height: u32,
    /// Stream `time_base` as a rational (same representation as [`crate::DecodedVideoFrame::timebase`] on frames from this source).
    pub timebase: Rational,
    /// Container reported duration in **seconds** when known.
    pub duration: Option<Rational>,
    /// Pixel format after negotiation (`YUV420P` / `NV12` / `RGBA` in v1).
    pub pixel_format: PixelFormat,
}

#[cfg(test)]
mod tests {
    use super::SourceInfo;
    use crate::{PixelFormat, Rational};

    fn sample_info() -> SourceInfo {
        SourceInfo {
            width: 640,
            height: 480,
            timebase: Rational::new_raw(1, 30_000),
            duration: Some(Rational::new_raw(60, 30)),
            pixel_format: PixelFormat::Yuv420p,
        }
    }

    #[test]
    fn source_info_eq_all_fields_match() {
        assert_eq!(sample_info(), sample_info());
    }

    #[test]
    fn source_info_ne_width() {
        let mut b = sample_info();
        b.width = 641;
        assert_ne!(sample_info(), b);
    }

    #[test]
    fn source_info_ne_height() {
        let mut b = sample_info();
        b.height = 360;
        assert_ne!(sample_info(), b);
    }

    #[test]
    fn source_info_ne_timebase() {
        let mut b = sample_info();
        b.timebase = Rational::new_raw(1, 60_000);
        assert_ne!(sample_info(), b);
    }

    #[test]
    fn source_info_ne_duration_some_vs_none() {
        let mut b = sample_info();
        b.duration = None;
        assert_ne!(sample_info(), b);
    }

    #[test]
    fn source_info_ne_duration_value() {
        let mut b = sample_info();
        b.duration = Some(Rational::new_raw(2, 1));
        assert_ne!(sample_info(), b);
    }

    #[test]
    fn source_info_ne_pixel_format() {
        let mut b = sample_info();
        b.pixel_format = PixelFormat::Nv12;
        assert_ne!(sample_info(), b);
    }

    #[test]
    fn source_info_clone_matches() {
        let a = sample_info();
        assert_eq!(a, a.clone());
    }

    #[test]
    fn source_info_debug_contains_dims() {
        let s = format!("{:?}", sample_info());
        assert!(s.contains("640"));
        assert!(s.contains("480"));
    }
}
