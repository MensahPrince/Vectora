use thiserror::Error;

/// Failures the engine can surface to a consumer. Wraps [`decoder::DecoderError`]
/// where the failure is purely a decoder concern so callers can distinguish
/// "engine bookkeeping broke" from "the codec said no".
#[derive(Debug, Error)]
pub enum EngineError {
    /// No source has been opened yet — `seek_*` was called before `open`.
    #[error("no source open")]
    NoSource,

    /// Underlying decoder failure (open / seek / decode).
    #[error("decoder failure")]
    Decoder(#[from] decoder::DecoderError),

    /// Worker thread has gone away — engine handle is no longer usable.
    #[error("engine worker thread is dead")]
    WorkerDead,
}
