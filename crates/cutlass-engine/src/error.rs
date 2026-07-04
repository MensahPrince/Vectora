use cutlass_core::DecodeError;
use cutlass_models::{ModelError, TimeError};
use cutlass_render::RenderError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Model(#[from] ModelError),

    #[error(transparent)]
    Time(#[from] TimeError),

    /// Preview/export failure from the render pipeline (decode, composite,
    /// encode, or I/O).
    #[error(transparent)]
    Render(#[from] RenderError),

    /// Native media probe/decode failure (unreadable or unsupported file),
    /// surfaced when importing or relinking media.
    #[error(transparent)]
    Decode(#[from] DecodeError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("import failed: {0}")]
    Import(String),

    #[error("export: {0}")]
    Export(String),

    #[error("media file not found: {0}")]
    MissingMedia(String),

    /// A command whose backend isn't available in this build (e.g. audio
    /// ducking and beat detection, which need the decoder's audio reader).
    #[error("unsupported on this build: {0}")]
    Unsupported(String),
}

impl EngineError {
    /// Stable machine-readable error category for the FFI/agent wire format
    /// (`{"kind": …, "message": …}`). Kinds are part of the protocol: shells
    /// branch on them (e.g. `missing_media` → relink flow), so renaming one is
    /// a breaking change.
    pub fn kind(&self) -> &'static str {
        match self {
            EngineError::Model(_) => "model",
            EngineError::Time(_) => "time",
            EngineError::Render(_) => "render",
            EngineError::Decode(_) => "decode",
            EngineError::Io(_) => "io",
            EngineError::Import(_) => "import",
            EngineError::Export(_) => "export",
            EngineError::MissingMedia(_) => "missing_media",
            EngineError::Unsupported(_) => "unsupported",
        }
    }
}
