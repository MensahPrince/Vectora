use thiserror::Error;

use crate::ids::{ClipId, MediaSourceId, TrackId};

/// Failures from timeline validation, commands, or (de)serialization.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TimelineError {
    #[error("track not found: {0}")]
    TrackNotFound(TrackId),

    #[error("clip not found: {0}")]
    ClipNotFound(ClipId),

    #[error("source not found: {0}")]
    SourceNotFound(MediaSourceId),

    #[error("source {source_id} is in use by clips: {by_clips:?}")]
    SourceInUse {
        source_id: MediaSourceId,
        by_clips: Vec<ClipId>,
    },

    #[error("clip would overlap {existing} at timeline position {attempted_position}")]
    ClipOverlap {
        existing: ClipId,
        attempted_position: String,
    },

    #[error("invalid trim: {0}")]
    InvalidTrim(&'static str),

    #[error("unsupported project schema {found} (max supported {supported_max})")]
    SchemaUnsupported { found: u32, supported_max: u32 },

    #[error("serde: {0}")]
    Serde(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_track_not_found() {
        let m = TimelineError::TrackNotFound(TrackId(7)).to_string();
        assert!(m.contains("track"));
        assert!(m.contains('7'));
    }

    #[test]
    fn display_clip_not_found() {
        let m = TimelineError::ClipNotFound(ClipId(3)).to_string();
        assert!(m.contains("clip"));
    }

    #[test]
    fn display_source_in_use_lists_clips() {
        let m = TimelineError::SourceInUse {
            source_id: MediaSourceId(1),
            by_clips: vec![ClipId(2), ClipId(5)],
        }
        .to_string();
        assert!(m.contains("in use"));
        assert!(m.contains('2'));
    }

    #[test]
    fn display_clip_overlap() {
        let m = TimelineError::ClipOverlap {
            existing: ClipId(1),
            attempted_position: "5/1".into(),
        }
        .to_string();
        assert!(m.contains("overlap"));
        assert!(m.contains("5/1"));
    }

    #[test]
    fn display_schema_unsupported() {
        let m = TimelineError::SchemaUnsupported {
            found: 9,
            supported_max: 1,
        }
        .to_string();
        assert!(m.contains('9'));
        assert!(m.contains('1'));
    }

    #[test]
    fn error_eq_variant() {
        assert_eq!(
            TimelineError::InvalidTrim("x"),
            TimelineError::InvalidTrim("x")
        );
        assert_ne!(
            TimelineError::InvalidTrim("x"),
            TimelineError::InvalidTrim("y")
        );
    }
}
