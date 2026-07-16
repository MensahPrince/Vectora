use std::io;
use std::path::PathBuf;

use thiserror::Error;

const BASE_EN_SPEC: ModelSpec = ModelSpec::new(
    "base.en",
    "ggml-base.en.bin",
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
    147_964_211,
    "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
    true,
    "about 148 MB on disk and about 388 MB RAM during inference",
);

/// A built-in Whisper model known to Cutlass.
///
/// This enum, rather than a caller-provided filename, is used by every public
/// [`ModelManager`] file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WhisperModel {
    /// The official English-only Whisper base model.
    BaseEn,
}

const WHISPER_MODELS: &[WhisperModel] = &[WhisperModel::BaseEn];

impl WhisperModel {
    /// Returns all models in stable catalog order.
    #[must_use]
    pub const fn catalog() -> &'static [Self] {
        WHISPER_MODELS
    }

    /// Returns the immutable catalog specification for this model.
    #[must_use]
    pub const fn spec(self) -> &'static ModelSpec {
        match self {
            Self::BaseEn => &BASE_EN_SPEC,
        }
    }
}

/// Immutable metadata for one verified model artifact.
///
/// Construction is intentionally private. Public model-manager operations can
/// therefore use only filenames compiled into this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    id: &'static str,
    filename: &'static str,
    url: &'static str,
    exact_bytes: u64,
    sha256: &'static str,
    english_only: bool,
    resource_description: &'static str,
}

impl ModelSpec {
    pub(super) const fn new(
        id: &'static str,
        filename: &'static str,
        url: &'static str,
        exact_bytes: u64,
        sha256: &'static str,
        english_only: bool,
        resource_description: &'static str,
    ) -> Self {
        Self {
            id,
            filename,
            url,
            exact_bytes,
            sha256,
            english_only,
            resource_description,
        }
    }

    /// Returns the stable application-facing model identifier.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Returns the fixed local filename.
    #[must_use]
    pub const fn filename(&self) -> &'static str {
        self.filename
    }

    /// Returns the official HTTPS artifact URL.
    #[must_use]
    pub const fn url(&self) -> &'static str {
        self.url
    }

    /// Returns the exact accepted artifact length.
    #[must_use]
    pub const fn exact_bytes(&self) -> u64 {
        self.exact_bytes
    }

    /// Returns the lowercase SHA-256 digest in hexadecimal.
    #[must_use]
    pub const fn sha256(&self) -> &'static str {
        self.sha256
    }

    /// Returns whether this model can recognize only English speech.
    #[must_use]
    pub const fn is_english_only(&self) -> bool {
        self.english_only
    }

    /// Returns an approximate disk and inference-memory description.
    #[must_use]
    pub const fn resource_description(&self) -> &'static str {
        self.resource_description
    }
}

/// A failure to establish a model download stream.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DownloadError {
    /// The server returned a non-success HTTP status.
    #[error("model server returned HTTP status {status}")]
    HttpStatus {
        /// The rejected HTTP status code.
        status: u16,
    },
    /// The HTTP transport failed before a usable stream was returned.
    #[error("model download transport failed: {message}")]
    Transport {
        /// A transport diagnostic without response content.
        message: String,
    },
    /// One or more configured timeouts were zero.
    #[error("model download timeouts must all be greater than zero")]
    InvalidTimeout,
    /// A custom downloader rejected the request.
    #[error("model downloader failed: {message}")]
    Custom {
        /// A custom downloader diagnostic.
        message: String,
    },
}

impl DownloadError {
    /// Creates an error for an injected downloader.
    #[must_use]
    pub fn custom(message: impl Into<String>) -> Self {
        Self::Custom {
            message: message.into(),
        }
    }
}

/// Why a regular model file failed exact integrity verification.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelIntegrityError {
    /// The file or stream length differed from the catalog.
    #[error("expected exactly {expected} bytes, found {actual}")]
    WrongSize {
        /// Exact catalogued bytes.
        expected: u64,
        /// Observed bytes.
        actual: u64,
    },
    /// The SHA-256 digest differed from the catalog.
    #[error("expected SHA-256 {expected}, found {actual}")]
    ChecksumMismatch {
        /// Catalogued lowercase SHA-256.
        expected: String,
        /// Observed lowercase SHA-256.
        actual: String,
    },
}

/// Current integrity state of one catalogued model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStatus {
    /// No model file exists.
    Missing,
    /// A regular file passed exact size and SHA-256 verification.
    Ready {
        /// Verified model path.
        path: PathBuf,
    },
    /// A regular file exists but failed integrity verification.
    Invalid {
        /// Invalid model path.
        path: PathBuf,
        /// Exact verification failure.
        reason: ModelIntegrityError,
    },
}

/// A model catalog, filesystem, download, or integrity failure.
#[derive(Debug, Error)]
pub enum ModelManagerError {
    /// Cooperative model installation cancellation was requested.
    #[error("model installation cancelled")]
    Cancelled,
    /// The configured model root was relative.
    #[error("model root must be absolute: `{root}`")]
    RootNotAbsolute {
        /// Rejected root.
        root: PathBuf,
    },
    /// The configured model root was a filesystem root.
    #[error("model root must not be a filesystem root: `{root}`")]
    FilesystemRoot {
        /// Rejected root.
        root: PathBuf,
    },
    /// The root contained lexical traversal.
    #[error("model root must not contain `.` or `..` traversal: `{root}`")]
    RootTraversal {
        /// Rejected root.
        root: PathBuf,
    },
    /// A compiled catalog entry violated path or digest invariants.
    #[error("invalid model catalog entry `{model_id}`: {detail}")]
    InvalidCatalogEntry {
        /// Stable catalog identifier.
        model_id: &'static str,
        /// Rejected invariant.
        detail: &'static str,
    },
    /// The model root was a link, Windows reparse point, or non-directory.
    #[error("unsafe model root `{root}`: {detail}")]
    UnsafeRoot {
        /// Rejected root path.
        root: PathBuf,
        /// Rejected filesystem type.
        detail: &'static str,
    },
    /// The lexical root stopped naming the pinned physical directory.
    #[error(
        "model root mapping changed while operating on `{root}`; expected pinned directory `{pinned_root}`"
    )]
    RootMappingChanged {
        /// Configured lexical root.
        root: PathBuf,
        /// Canonical directory pinned at operation start.
        pinned_root: PathBuf,
    },
    /// A known model path was a link, Windows reparse point, or non-regular file.
    #[error("unsafe model file `{path}`: {detail}")]
    UnsafeModelFile {
        /// Rejected known model path.
        path: PathBuf,
        /// Rejected filesystem type.
        detail: &'static str,
    },
    /// A model or temporary file name changed objects during an operation.
    #[error("{object} changed during the operation: `{path}`")]
    FilesystemObjectChanged {
        /// Path used for diagnostics.
        path: PathBuf,
        /// Kind of filesystem object that changed.
        object: &'static str,
    },
    /// A required model file did not exist.
    #[error("model file is missing: `{path}`")]
    MissingModel {
        /// Expected known model path.
        path: PathBuf,
    },
    /// A model file or completed download failed integrity verification.
    #[error("model integrity check failed for `{path}`: {reason}")]
    Integrity {
        /// Known model path.
        path: PathBuf,
        /// Exact verification failure.
        #[source]
        reason: ModelIntegrityError,
    },
    /// A stream exceeded the catalogued byte maximum.
    #[error("model download exceeded {expected} bytes after observing {observed}")]
    DownloadTooLarge {
        /// Exact catalogued maximum.
        expected: u64,
        /// Bytes observed when overflow was detected.
        observed: u64,
    },
    /// A downloader could not produce a stream.
    #[error(transparent)]
    Download(#[from] DownloadError),
    /// A bounded filesystem or stream operation failed.
    #[error("failed to {operation} `{path}`: {source}")]
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Relevant filesystem path.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// Installation completed, but the containing directory could not be synced.
    #[error(
        "verified model was installed at `{path}`, but durability confirmation for its containing directory failed; retrying ensure is safe: {source}"
    )]
    InstalledButDirectorySyncFailed {
        /// Installed verified model path.
        path: PathBuf,
        /// Directory synchronization failure.
        #[source]
        source: io::Error,
    },
    /// Unique temporary filenames could not be allocated.
    #[error("could not allocate a unique temporary model file in `{root}`")]
    TemporaryNameExhausted {
        /// Model directory containing collisions.
        root: PathBuf,
    },
}
