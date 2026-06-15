//! On-demand model weights: resolve, download, and verify under
//! `~/.cutlass/models/`.
//!
//! Models are *data*, not code — weights are never bundled into the binary or
//! a project file. A [`ModelSpec`] names a downloadable file with its SHA-256;
//! [`ModelCache::ensure`] returns the local path, fetching on first use and
//! verifying the checksum (streamed as it downloads) before installing it
//! atomically via a `.part` rename. Pure resolution + verification unit-test
//! without a network (the canonical `SHA-256("abc")` vector and the
//! present-valid short-circuit); the model *registry* (real URLs + checksums)
//! lands with the whisper.cpp backend that consumes it, so no checksum is
//! invented ahead of a real file.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

const DOWNLOAD_BLOCK: usize = 64 * 1024;

/// `~/.cutlass/models/` (HOME-relative; falls back to the working directory
/// when HOME is unset, mirroring `recent.json`, autosave, and `config.toml`).
pub fn models_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cutlass")
        .join("models")
}

/// A downloadable model file: how it's named, where it comes from, and how to
/// know it arrived intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// Stable short name used in config and logs, e.g. `"whisper-base.en"`.
    pub name: &'static str,
    /// File name on disk and cache key, e.g. `"ggml-base.en.bin"`.
    pub file: &'static str,
    /// Download URL.
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: &'static str,
    /// Expected size in bytes (download-progress denominator; `0` = unknown).
    pub size: u64,
}

/// Tiny English whisper model (~75 MB): fastest, lowest accuracy.
pub const WHISPER_TINY_EN: ModelSpec = ModelSpec {
    name: "tiny.en",
    file: "ggml-tiny.en.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
    sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    size: 77_704_715,
};

/// Base English whisper model (~142 MB): the default — a good speed/accuracy
/// balance and the `[ml] transcribe_model` default.
pub const WHISPER_BASE_EN: ModelSpec = ModelSpec {
    name: "base.en",
    file: "ggml-base.en.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
    sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
    size: 147_964_211,
};

/// Small English whisper model (~466 MB): slower, higher accuracy.
pub const WHISPER_SMALL_EN: ModelSpec = ModelSpec {
    name: "small.en",
    file: "ggml-small.en.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
    sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
    size: 487_614_201,
};

/// Known whisper.cpp ggml models, smallest first. The official
/// `ggerganov/whisper.cpp` builds on Hugging Face; checksums are the git-LFS
/// OIDs (authoritative SHA-256s), so [`ModelCache::ensure`] verifies a real
/// download against a real hash.
pub const WHISPER_MODELS: &[ModelSpec] = &[WHISPER_TINY_EN, WHISPER_BASE_EN, WHISPER_SMALL_EN];

/// Look up a whisper model by its `[ml]` `transcribe_model` name (e.g.
/// `"base.en"`).
pub fn whisper_model(name: &str) -> Option<ModelSpec> {
    WHISPER_MODELS.iter().copied().find(|m| m.name == name)
}

/// Model-cache failures, kept distinct so the UI can tell a checksum mismatch
/// (re-download) from a network error (retry later) from a cancel.
#[derive(Debug, thiserror::Error)]
pub enum ModelCacheError {
    #[error("model cache I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not download {url}: {message}")]
    Download { url: String, message: String },
    #[error("checksum mismatch for {name}: expected {expected}, got {actual}")]
    Checksum {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("model download cancelled")]
    Cancelled,
}

/// Resolves and caches model weights in a directory.
#[derive(Debug, Clone)]
pub struct ModelCache {
    dir: PathBuf,
}

impl Default for ModelCache {
    /// A cache rooted at [`models_dir`].
    fn default() -> Self {
        Self::new(models_dir())
    }
}

impl ModelCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where `spec` lives (or will live) on disk.
    pub fn path(&self, spec: &ModelSpec) -> PathBuf {
        self.dir.join(spec.file)
    }

    /// True when the file is present and its checksum matches `spec`.
    pub fn is_ready(&self, spec: &ModelSpec) -> bool {
        verify_file(&self.path(spec), spec.sha256).unwrap_or(false)
    }

    /// The local path to `spec`, downloading + verifying on first use. A
    /// present, valid file returns immediately (progress `1.0`, no network).
    /// `on_progress` receives the downloaded fraction in `[0, 1]` while
    /// fetching. The download streams to a `.part` sidecar and is renamed into
    /// place only after the checksum matches, so a crash mid-download never
    /// leaves a corrupt model that looks complete.
    pub fn ensure(
        &self,
        spec: &ModelSpec,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(f32),
    ) -> Result<PathBuf, ModelCacheError> {
        let path = self.path(spec);
        if verify_file(&path, spec.sha256)? {
            on_progress(1.0);
            return Ok(path);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(ModelCacheError::Cancelled);
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension("part");
        let actual =
            download_to(spec.url, &tmp, spec.size, cancel, on_progress).inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })?;
        if !actual.eq_ignore_ascii_case(spec.sha256) {
            let _ = std::fs::remove_file(&tmp);
            return Err(ModelCacheError::Checksum {
                name: spec.name.to_string(),
                expected: spec.sha256.to_string(),
                actual,
            });
        }
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }
}

/// Stream `url` into `tmp`, hashing as it goes and reporting progress against
/// `expected_size`. Returns the file's lowercase hex SHA-256.
fn download_to(
    url: &str,
    tmp: &Path,
    expected_size: u64,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(f32),
) -> Result<String, ModelCacheError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(ModelCacheError::Cancelled);
    }
    let response = ureq::get(url)
        .call()
        .map_err(|e| ModelCacheError::Download {
            url: url.to_string(),
            message: e.to_string(),
        })?;
    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; DOWNLOAD_BLOCK];
    let mut done: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ModelCacheError::Cancelled);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        done += n as u64;
        if expected_size > 0 {
            on_progress((done as f32 / expected_size as f32).min(1.0));
        }
    }
    file.flush()?;
    Ok(hex_lower(&hasher.finalize()))
}

/// True when `path` exists and its SHA-256 equals `sha256_hex`
/// (case-insensitive). A missing file is `Ok(false)`, not an error.
pub fn verify_file(path: &Path, sha256_hex: &str) -> Result<bool, ModelCacheError> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; DOWNLOAD_BLOCK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()).eq_ignore_ascii_case(sha256_hex))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical SHA-256 of the bytes `b"abc"` (FIPS 180-2 test vector).
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn spec(file: &'static str, sha: &'static str) -> ModelSpec {
        ModelSpec {
            name: "test-model",
            file,
            // Never reached in tests: ensure short-circuits on a valid file
            // and bails on the cancel flag before any request.
            url: "http://model.invalid/never",
            sha256: sha,
            size: 3,
        }
    }

    #[test]
    fn models_dir_lives_under_dot_cutlass() {
        let dir = models_dir();
        assert!(
            dir.ends_with(PathBuf::from(".cutlass").join("models")),
            "{dir:?}"
        );
    }

    #[test]
    fn verify_file_matches_the_known_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert!(verify_file(&path, ABC_SHA256).unwrap());
    }

    #[test]
    fn verify_file_is_case_insensitive_and_rejects_mismatch_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.bin");
        std::fs::write(&path, b"abc").unwrap();

        assert!(verify_file(&path, &ABC_SHA256.to_uppercase()).unwrap());
        assert!(!verify_file(&path, "00").unwrap());
        std::fs::write(&path, b"abd").unwrap();
        assert!(!verify_file(&path, ABC_SHA256).unwrap());
        assert!(!verify_file(&dir.path().join("nope.bin"), ABC_SHA256).unwrap());
    }

    #[test]
    fn ensure_short_circuits_on_a_present_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        let spec = spec("model.bin", ABC_SHA256);
        std::fs::write(cache.path(&spec), b"abc").unwrap();

        assert!(cache.is_ready(&spec));
        let mut progress = Vec::new();
        let path = cache
            .ensure(&spec, &AtomicBool::new(false), &mut |p| progress.push(p))
            .unwrap();
        assert_eq!(path, dir.path().join("model.bin"));
        assert_eq!(
            progress.last(),
            Some(&1.0),
            "no network, immediate completion"
        );
    }

    #[test]
    fn whisper_registry_lookup_and_integrity() {
        assert_eq!(whisper_model("base.en"), Some(WHISPER_BASE_EN));
        assert_eq!(whisper_model("tiny.en"), Some(WHISPER_TINY_EN));
        assert!(whisper_model("does-not-exist").is_none());
        for m in WHISPER_MODELS {
            assert_eq!(m.sha256.len(), 64, "{}: sha256 is 64 hex chars", m.name);
            assert!(
                m.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{}: sha256 is hex",
                m.name
            );
            assert!(
                m.url
                    .starts_with("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-"),
                "{}: official HF url",
                m.name
            );
            assert!(m.size > 0, "{}: known size", m.name);
        }
    }

    #[test]
    fn ensure_bails_on_cancel_before_touching_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        // Absent file + raised cancel: ensure must return Cancelled without
        // attempting to reach the (invalid) URL.
        let err = cache
            .ensure(
                &spec("absent.bin", ABC_SHA256),
                &AtomicBool::new(true),
                &mut |_| {},
            )
            .unwrap_err();
        assert!(matches!(err, ModelCacheError::Cancelled));
    }
}
