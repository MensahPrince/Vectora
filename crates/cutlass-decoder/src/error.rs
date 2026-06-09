use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("failed to open media")]
    Open(#[source] ffmpeg_next::Error),

    #[error("demuxer read failed")]
    Io(#[source] ffmpeg_next::Error),

    #[error("decode failed")]
    Decode(#[source] ffmpeg_next::Error),

    #[error("unsupported: {what}")]
    Unsupported { what: String },

    #[error("hardware acceleration unavailable: {accel}")]
    HwAccelUnavailable { accel: &'static str },
}

impl DecodeError {
    pub fn unsupported(what: impl Into<String>) -> Self {
        DecodeError::Unsupported {
            what: what.into(),
        }
    }
}
