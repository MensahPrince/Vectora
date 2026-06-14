//! Shared helpers for `cutlass-models` integration tests.

#![allow(dead_code)]

use cutlass_models::{MediaSource, Project, Rational, RationalTime, TimeRange, TrackId, TrackKind};

pub const FPS_24: Rational = Rational::FPS_24;
pub const FPS_30: Rational = Rational::FPS_30;
pub const FPS_23_976: Rational = Rational::FPS_23_976;

pub fn rt(value: i64) -> RationalTime {
    RationalTime::new(value, FPS_24)
}

pub fn rt_at(value: i64, rate: Rational) -> RationalTime {
    RationalTime::new(value, rate)
}

pub fn tr(start: i64, duration: i64) -> TimeRange {
    TimeRange::at_rate(start, duration, FPS_24)
}

pub fn tr_at(start: i64, duration: i64, rate: Rational) -> TimeRange {
    TimeRange::at_rate(start, duration, rate)
}

pub fn sample_media(fps: Rational, duration: i64) -> MediaSource {
    MediaSource::new("/tmp/sample.mp4", 3840, 2160, fps, duration, true)
}

pub fn project_with_media(duration: i64) -> (Project, cutlass_models::MediaId, TrackId) {
    let mut project = Project::new("test", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, duration));
    let track = project.add_track(TrackKind::Video, "V1");
    (project, media_id, track)
}
