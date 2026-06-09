use crate::error::ModelError;
use crate::ids::{ClipId, MediaId};
use crate::time::{RationalTime, TimeRange, resample, time_add, time_sub};

/// What a clip draws. Either a trimmed range of imported media, or synthetic
/// content rendered by the engine (text, shapes, solids, ...).
#[derive(Debug, Clone, PartialEq)]
pub enum ClipSource {
    /// A trimmed portion of a [`MediaSource`](crate::MediaSource).
    ///
    /// `source` is the in/out within the media at the media's native rate.
    Media { media: MediaId, source: TimeRange },
    /// Engine-generated content with no backing file.
    Generated(Generator),
}

/// A synthetic clip with no source media. Parameters are intentionally minimal
/// for now; richer styling (fonts, transforms, gradients) can be added per
/// variant without touching the timeline model.
#[derive(Debug, Clone, PartialEq)]
pub enum Generator {
    /// A title / text layer.
    Text { content: String },
    /// A solid fill (RGBA, 0-255).
    SolidColor { rgba: [u8; 4] },
    /// A vector shape.
    Shape { shape: Shape },
    /// A pass-through layer that only affects tracks beneath it.
    Adjustment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Rectangle,
    Ellipse,
}

/// A placement of some [`ClipSource`] on a track.
///
/// `timeline` is where the clip sits on the sequence, at the timeline rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub id: ClipId,
    pub content: ClipSource,
    pub timeline: TimeRange,
}

impl Clip {
    /// A clip backed by a trimmed range of imported media.
    pub fn from_media(media: MediaId, source: TimeRange, timeline: TimeRange) -> Self {
        Self {
            id: ClipId::next(),
            content: ClipSource::Media { media, source },
            timeline,
        }
    }

    /// A generated clip (text, shape, solid, ...).
    pub fn generated(generator: Generator, timeline: TimeRange) -> Self {
        Self {
            id: ClipId::next(),
            content: ClipSource::Generated(generator),
            timeline,
        }
    }

    /// Timeline start position.
    pub fn start(&self) -> RationalTime {
        self.timeline.start
    }

    /// Exclusive timeline end.
    pub fn end(&self) -> Result<RationalTime, ModelError> {
        self.timeline.end()
    }

    /// The media this clip references, or `None` for generated content.
    pub fn media(&self) -> Option<MediaId> {
        match &self.content {
            ClipSource::Media { media, .. } => Some(*media),
            ClipSource::Generated(_) => None,
        }
    }

    /// The source in/out range, or `None` for generated content.
    pub fn source_range(&self) -> Option<TimeRange> {
        match &self.content {
            ClipSource::Media { source, .. } => Some(*source),
            ClipSource::Generated(_) => None,
        }
    }

    pub fn is_generated(&self) -> bool {
        matches!(self.content, ClipSource::Generated(_))
    }

    /// Map a timeline position to the corresponding source time, for media clips.
    pub fn source_time_at(&self, timeline_pos: RationalTime) -> Result<Option<RationalTime>, ModelError> {
        if !self.timeline.contains(timeline_pos)? {
            return Ok(None);
        }
        match &self.content {
            ClipSource::Media { source, .. } => {
                let offset_tl = time_sub(&timeline_pos, &self.timeline.start)?;
                let offset_src = resample(offset_tl, source.start.rate);
                Ok(Some(time_add(&source.start, &offset_src)?))
            }
            ClipSource::Generated(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Rational;

    const R24: Rational = Rational::FPS_24;
    const R30: Rational = Rational::FPS_30;

    fn rt(value: i64, rate: Rational) -> RationalTime {
        RationalTime::new(value, rate)
    }

    fn tr(start: i64, duration: i64, rate: Rational) -> TimeRange {
        TimeRange::at_rate(start, duration, rate)
    }

    fn media_clip(
        media: MediaId,
        source: TimeRange,
        timeline: TimeRange,
    ) -> Clip {
        Clip::from_media(media, source, timeline)
    }

    // --- constructors -----------------------------------------------------

    #[test]
    fn from_media_wires_content_and_timeline() {
        let media = MediaId::from_raw(42);
        let source = tr(100, 50, R30);
        let timeline = tr(10, 40, R24);
        let clip = media_clip(media, source, timeline);

        assert_eq!(
            clip.content,
            ClipSource::Media {
                media,
                source,
            }
        );
        assert_eq!(clip.timeline, timeline);
        assert!(!clip.is_generated());
    }

    #[test]
    fn from_media_assigns_distinct_ids() {
        let media = MediaId::from_raw(1);
        let source = tr(0, 10, R24);
        let timeline = tr(0, 10, R24);
        let a = media_clip(media, source, timeline);
        let b = media_clip(media, source, timeline);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn generated_text_clip() {
        let timeline = tr(0, 48, R24);
        let clip = Clip::generated(
            Generator::Text {
                content: "Hello".into(),
            },
            timeline,
        );
        assert_eq!(
            clip.content,
            ClipSource::Generated(Generator::Text {
                content: "Hello".into(),
            })
        );
        assert_eq!(clip.timeline, timeline);
        assert!(clip.is_generated());
    }

    #[test]
    fn generated_all_variants() {
        let timeline = tr(0, 10, R24);

        let solid = Clip::generated(
            Generator::SolidColor {
                rgba: [255, 0, 0, 255],
            },
            timeline,
        );
        assert!(matches!(
            solid.content,
            ClipSource::Generated(Generator::SolidColor { .. })
        ));

        let shape = Clip::generated(
            Generator::Shape {
                shape: Shape::Ellipse,
            },
            timeline,
        );
        assert!(matches!(
            shape.content,
            ClipSource::Generated(Generator::Shape {
                shape: Shape::Ellipse
            })
        ));

        let adj = Clip::generated(Generator::Adjustment, timeline);
        assert!(matches!(
            adj.content,
            ClipSource::Generated(Generator::Adjustment)
        ));
    }

    #[test]
    fn generated_assigns_distinct_ids() {
        let timeline = tr(0, 10, R24);
        let a = Clip::generated(Generator::Adjustment, timeline);
        let b = Clip::generated(Generator::Adjustment, timeline);
        assert_ne!(a.id, b.id);
    }

    // --- accessors --------------------------------------------------------

    #[test]
    fn media_clip_accessors() {
        let media = MediaId::from_raw(7);
        let source = tr(50, 25, R24);
        let timeline = tr(100, 25, R24);
        let clip = media_clip(media, source, timeline);

        assert_eq!(clip.media(), Some(media));
        assert_eq!(clip.source_range(), Some(source));
        assert_eq!(clip.start(), rt(100, R24));
        assert_eq!(clip.end().unwrap(), rt(125, R24));
    }

    #[test]
    fn generated_clip_accessors_are_none() {
        let clip = Clip::generated(
            Generator::Text {
                content: "x".into(),
            },
            tr(5, 10, R24),
        );
        assert_eq!(clip.media(), None);
        assert_eq!(clip.source_range(), None);
        assert_eq!(clip.start().value, 5);
        assert_eq!(clip.end().unwrap().value, 15);
    }

    #[test]
    fn clip_clone_and_eq() {
        let media = MediaId::from_raw(1);
        let source = tr(0, 10, R24);
        let timeline = tr(0, 10, R24);
        let a = media_clip(media, source, timeline);
        let b = a.clone();
        assert_eq!(a, b);
        assert_eq!(a.id, b.id);
    }

    // --- source_time_at: same-rate media ----------------------------------

    #[test]
    fn source_time_at_same_rate_maps_one_to_one() {
        // source [100, 110) placed at timeline [10, 20) — 1:1 at 24fps.
        let clip = media_clip(
            MediaId::from_raw(1),
            tr(100, 10, R24),
            tr(10, 10, R24),
        );

        assert_eq!(
            clip.source_time_at(rt(15, R24)).unwrap(),
            Some(rt(105, R24))
        );
        assert_eq!(
            clip.source_time_at(rt(10, R24)).unwrap(),
            Some(rt(100, R24))
        );
        assert_eq!(
            clip.source_time_at(rt(19, R24)).unwrap(),
            Some(rt(109, R24))
        );
    }

    #[test]
    fn source_time_at_half_open_boundaries() {
        let clip = media_clip(
            MediaId::from_raw(1),
            tr(0, 10, R24),
            tr(10, 10, R24),
        );

        // Exclusive end is not contained.
        assert_eq!(clip.source_time_at(rt(20, R24)).unwrap(), None);
        // Before start.
        assert_eq!(clip.source_time_at(rt(9, R24)).unwrap(), None);
        // After end.
        assert_eq!(clip.source_time_at(rt(21, R24)).unwrap(), None);
    }

    #[test]
    fn source_time_at_generated_always_none() {
        let clip = Clip::generated(
            Generator::Text {
                content: "title".into(),
            },
            tr(0, 100, R24),
        );
        assert_eq!(clip.source_time_at(rt(50, R24)).unwrap(), None);
    }

    // --- source_time_at: mixed rates ------------------------------------

    #[test]
    fn source_time_at_resamples_across_rates() {
        // 120 source ticks @ 30fps -> 96 timeline ticks @ 24fps.
        let clip = media_clip(
            MediaId::from_raw(1),
            tr(0, 120, R30),
            tr(0, 96, R24),
        );

        // Timeline midpoint should land near source midpoint after resample.
        let src = clip.source_time_at(rt(48, R24)).unwrap().unwrap();
        assert_eq!(src.rate, R30);
        // 48 @ 24fps = 60 @ 30fps offset from source start 0.
        assert_eq!(src.value, 60);

        // Timeline start maps to source start regardless of rate.
        assert_eq!(
            clip.source_time_at(rt(0, R24)).unwrap(),
            Some(rt(0, R30))
        );
    }

    #[test]
    fn source_time_at_offset_from_nonzero_source_start() {
        // source [200, 300) @ 30fps at timeline [0, 80) @ 24fps.
        let clip = media_clip(
            MediaId::from_raw(1),
            tr(200, 100, R30),
            tr(0, 80, R24),
        );

        let at_start = clip.source_time_at(rt(0, R24)).unwrap().unwrap();
        assert_eq!(at_start, rt(200, R30));

        // 40 timeline ticks @ 24fps -> 50 source ticks @ 30fps from in-point.
        let mid = clip.source_time_at(rt(40, R24)).unwrap().unwrap();
        assert_eq!(mid, rt(250, R30));
    }

    // --- source_time_at: errors -------------------------------------------

    #[test]
    fn source_time_at_rate_mismatch_errors() {
        let clip = media_clip(
            MediaId::from_raw(1),
            tr(0, 10, R24),
            tr(0, 10, R24),
        );
        let err = clip.source_time_at(rt(5, R30)).unwrap_err();
        assert_eq!(
            err,
            ModelError::RateMismatch {
                expected: R30,
                got: R24,
            }
        );
    }
}
