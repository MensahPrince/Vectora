use thiserror::Error;

use crate::ids::SourceId;
use decoder::DecoderError;

/// Failures specific to the engine layer or bubbled from [`Decoder`](decoder::Decoder).
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("source not found: {0}")]
    SourceNotFound(SourceId),

    #[error("source already open: {0}")]
    SourceAlreadyOpen(SourceId),

    #[error("decoder failure")]
    Decoder(#[from] DecoderError),

    #[error("worker thread is dead — engine handle is invalid")]
    WorkerDead,
}

#[cfg(test)]
mod tests {
    use super::EngineError;
    use crate::ids::SourceId;
    use decoder::DecoderError;

    #[test]
    fn display_source_not_found() {
        let m = EngineError::SourceNotFound(SourceId(9)).to_string();
        assert!(m.contains("not found"), "{m}");
        assert!(m.contains('9'), "{m}");
    }

    #[test]
    fn display_source_already_open() {
        let m = EngineError::SourceAlreadyOpen(SourceId(3)).to_string();
        assert!(m.contains("already open"), "{m}");
    }

    #[test]
    fn display_decoder_variant() {
        let inner = DecoderError::unsupported("test codec");
        let m = EngineError::Decoder(inner).to_string();
        assert!(m.contains("decoder"), "{m}");
    }

    #[test]
    fn display_worker_dead() {
        let m = EngineError::WorkerDead.to_string();
        assert!(m.contains("worker"), "{m}");
    }
}
