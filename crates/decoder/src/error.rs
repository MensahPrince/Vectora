use thiserror::Error;

/// Recoverable / fatal failures while opening, seeking, or decoding. See [`DecodeOutcome::Eof`](crate::DecodeOutcome::Eof) for normal end-of-stream.
#[derive(Debug, Error)]
pub enum DecoderError {
    /// Demuxer open, stream selection, or parameter setup failed.
    #[error("failed to open or probe media")]
    Open(#[source] ffmpeg_next::Error),

    /// `avformat_seek_file` (or equivalent) failed.
    #[error("seek failed")]
    Seek(#[source] ffmpeg_next::Error),

    /// Demuxer packet read after the file was opened (yanked disk, corrupt mid-stream, network source, …).
    /// Prefer distinct recovery from [`DecoderError::Decode`] (codec) and [`DecoderError::Open`] (probe).
    #[error("demuxer read failed")]
    Io(#[source] ffmpeg_next::Error),

    /// `avcodec_send_packet` / `avcodec_receive_frame` failed.
    #[error("decode failed")]
    Decode(#[source] ffmpeg_next::Error),

    /// Codec, pixel format, or path not supported in v1.
    #[error("unsupported: {what}")]
    Unsupported { what: String },

    /// OS-level I/O (not FFmpeg libavformat read errors).
    #[error("OS I/O error")]
    StdIo(#[from] std::io::Error),
}

impl DecoderError {
    pub fn unsupported(what: impl Into<String>) -> Self {
        DecoderError::Unsupported {
            what: what.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DecoderError;
    use ffmpeg_next::Error as Fe;

    #[test]
    fn display_unsupported() {
        let e = DecoderError::unsupported("pixel fmt XYZ");
        assert!(e.to_string().contains("pixel fmt"));
    }

    #[test]
    fn display_open_wrapper_message() {
        let e = DecoderError::Open(Fe::DemuxerNotFound);
        assert!(
            e.to_string().contains("open") || e.to_string().contains("probe"),
            "{}",
            e
        );
    }

    #[test]
    fn display_seek_wrapper_message() {
        let e = DecoderError::Seek(Fe::Bug);
        assert!(e.to_string().contains("seek"), "{e}");
    }

    #[test]
    fn display_decode_wrapper_message() {
        let e = DecoderError::Decode(Fe::InvalidData);
        assert!(e.to_string().contains("decode"), "{e}");
    }

    #[test]
    fn display_io_wrapper_message() {
        let e = DecoderError::Io(Fe::Eof);
        assert!(e.to_string().contains("demuxer"), "{e}");
    }

    #[test]
    fn display_stdio_wrapper_message() {
        let e = DecoderError::StdIo(std::io::Error::other("disk full"));
        assert!(e.to_string().contains("OS"), "{e}");
    }

    #[test]
    fn unsupported_from_string_literal() {
        let e = DecoderError::unsupported("x");
        match e {
            DecoderError::Unsupported { what } => assert_eq!(what, "x"),
            _ => panic!("variant"),
        }
    }

    #[test]
    fn unsupported_from_owned_string() {
        let msg = String::from("reason");
        let e = DecoderError::unsupported(msg.clone());
        match e {
            DecoderError::Unsupported { what } => assert_eq!(what, msg),
            _ => panic!("variant"),
        }
    }

    #[test]
    fn debug_lists_stdio_kind() {
        let e = DecoderError::StdIo(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "x",
        ));
        assert!(format!("{e:?}").contains("StdIo"));
    }
}
