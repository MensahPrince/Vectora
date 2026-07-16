use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use cap_std::ambient_authority;
use cap_std::fs::{
    Dir as CapabilityDir, File as CapabilityFile, OpenOptions as CapabilityOpenOptions,
};
use same_file::Handle as FileIdentity;
use sha2::{Digest, Sha256};

use super::catalog::{
    ModelIntegrityError, ModelManagerError, ModelSpec, ModelStatus, WhisperModel,
};
use super::download::{HttpDownloader, ModelDownloader};
use super::filesystem::{
    FileVersion, TEMP_FILE_SEQUENCE, check_begin_commit, check_cancelled, digest_to_hex,
    ensure_cap_regular_model_file, ensure_directory_root, ensure_regular_model_file,
    integrity_error, lock_model_with_cancellation, model_lock, never_cancelled, next_read_length,
    validate_root, validate_spec,
};

const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024;
const TEMP_FILE_ATTEMPTS: u64 = 256;

/// Safe, transactional storage for built-in Whisper models.
///
/// Every disk operation pins a canonical root directory handle, performs
/// model I/O relative to that handle, and revalidates that the configured root
/// still names the pinned directory before reporting success or mutating a
/// known model name.
#[derive(Debug, Clone)]
pub struct ModelManager {
    root: PathBuf,
}

impl ModelManager {
    /// Validates and stores an absolute, non-root model directory.
    ///
    /// This constructor performs lexical validation only and does not create
    /// the directory. The final root itself is rejected later if it is a
    /// symlink or non-directory object.
    ///
    /// # Errors
    ///
    /// Returns [`ModelManagerError`] for a relative path, filesystem root, or
    /// explicit lexical traversal.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ModelManagerError> {
        Ok(Self {
            root: validate_root(root.as_ref())?,
        })
    }

    /// Returns the validated model directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the fixed path for a built-in model without touching the disk.
    ///
    /// # Errors
    ///
    /// Returns an error only if a compiled catalog invariant is invalid.
    pub fn model_path(&self, model: WhisperModel) -> Result<PathBuf, ModelManagerError> {
        self.path_for_spec(model.spec())
    }

    /// Verifies the known file, returning missing, ready, or invalid status.
    ///
    /// Links, Windows reparse points, and non-regular model files are rejected
    /// as errors rather than represented as ordinary corruption.
    ///
    /// # Errors
    ///
    /// Returns [`ModelManagerError`] for unsafe filesystem objects or I/O
    /// failures.
    pub fn status(&self, model: WhisperModel) -> Result<ModelStatus, ModelManagerError> {
        self.status_spec(model.spec())
    }

    /// Requires exact size and SHA-256 verification and returns the model path.
    ///
    /// # Errors
    ///
    /// Returns [`ModelManagerError::MissingModel`], an integrity error, an
    /// unsafe-file error, or an I/O error.
    pub fn verify(&self, model: WhisperModel) -> Result<PathBuf, ModelManagerError> {
        self.verify_spec(model.spec())
    }

    /// Ensures a verified model using the bounded default HTTPS downloader.
    ///
    /// # Errors
    ///
    /// Returns [`ModelManagerError`] when verification, downloading, durable
    /// writing, or atomic installation fails.
    pub fn ensure(&self, model: WhisperModel) -> Result<PathBuf, ModelManagerError> {
        self.ensure_with(model, &HttpDownloader::default())
    }

    /// Ensures a verified model using an injected streaming downloader.
    ///
    /// Concurrent calls for the same canonical model path are serialized
    /// within the process, including calls through lexical directory aliases.
    /// Existing files are reused only after full verification.
    ///
    /// # Errors
    ///
    /// Returns [`ModelManagerError`] when verification, downloading, durable
    /// writing, or atomic installation fails.
    pub fn ensure_with(
        &self,
        model: WhisperModel,
        downloader: &dyn ModelDownloader,
    ) -> Result<PathBuf, ModelManagerError> {
        self.ensure_with_cancellation(model, downloader, &never_cancelled)
    }

    /// Ensures a verified model with cooperative cancellation.
    ///
    /// `cancelled` is borrowed for this call, so it may directly borrow a job
    /// context. It can be called many times and should be inexpensive. A panic
    /// from the callback is treated as a cancellation request.
    ///
    /// Cancellation is checked around root setup, lock acquisition, existing
    /// model verification, stream establishment, each download read, and
    /// durable temporary-file writes. A single blocking transport or reader
    /// call, and the existing-model hash pass, cannot be interrupted midway.
    ///
    /// The final cancellation check is used as this standalone API's commit
    /// decision. Once it passes, cancellation is no longer consulted:
    /// replacement, durability confirmation, and root-mapping checks finish
    /// and return their actual result. An outer job that must atomically
    /// coordinate cancellation with publication should instead use
    /// [`Self::ensure_with_cancellation_and_commit`] and bind its commit
    /// callback to the job owner's atomic commit handshake.
    ///
    /// # Errors
    ///
    /// Returns [`ModelManagerError::Cancelled`] when cancellation is observed
    /// before commit. Other failures retain the same meaning as
    /// [`Self::ensure_with`].
    pub fn ensure_with_cancellation(
        &self,
        model: WhisperModel,
        downloader: &dyn ModelDownloader,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<PathBuf, ModelManagerError> {
        let begin_commit = || check_cancelled(cancelled).is_ok();
        self.ensure_with_cancellation_and_commit(model, downloader, cancelled, &begin_commit)
    }

    /// Ensures a verified model with cancellation and an atomic commit handshake.
    ///
    /// Both callbacks are borrowed for this call and may directly borrow an
    /// outer job context. `cancelled` has the same cooperative semantics as in
    /// [`Self::ensure_with_cancellation`]: it may be called many times before
    /// publication and a panic is treated as cancellation.
    ///
    /// `begin_commit` is not called when a verified existing model is reused.
    /// For a download or replacement, it is called exactly once, after the
    /// complete stream has passed exact length and SHA-256 verification, the
    /// temporary file has been flushed and synced, temporary-file identity and
    /// replacement-target safety have been checked, and the configured root
    /// mapping has been revalidated. The call occurs immediately before the
    /// first target removal or rename.
    ///
    /// Returning `false`, or panicking, fails closed with
    /// [`ModelManagerError::Cancelled`]. The temporary file is cleaned up and
    /// an existing target is left unchanged. Returning `true` is the point of
    /// no return: neither callback is consulted again, and replacement,
    /// directory durability, and final root-mapping checks return their actual
    /// result.
    ///
    /// An outer job manager can remove the cancellation/publication race by
    /// making `begin_commit` perform its atomic transition from cancellable to
    /// committing (for example, by calling its `try_begin_commit` operation).
    ///
    /// # Errors
    ///
    /// Returns [`ModelManagerError::Cancelled`] when cancellation is observed
    /// before commit or the commit callback rejects or panics. Other failures
    /// retain the same meaning as [`Self::ensure_with`].
    pub fn ensure_with_cancellation_and_commit(
        &self,
        model: WhisperModel,
        downloader: &dyn ModelDownloader,
        cancelled: &dyn Fn() -> bool,
        begin_commit: &dyn Fn() -> bool,
    ) -> Result<PathBuf, ModelManagerError> {
        self.ensure_spec_with_cancellation_and_commit(
            model.spec(),
            downloader,
            cancelled,
            begin_commit,
        )
    }

    /// Removes only the known regular model file.
    ///
    /// Returns `true` when a file was removed and `false` when it was already
    /// absent. Directories are never removed. Link and Windows reparse-point
    /// model files are rejected and left untouched.
    ///
    /// # Errors
    ///
    /// Returns [`ModelManagerError`] for unsafe filesystem objects or I/O
    /// failures.
    pub fn remove(&self, model: WhisperModel) -> Result<bool, ModelManagerError> {
        self.remove_spec(model.spec())
    }

    pub(super) fn status_spec(&self, spec: &ModelSpec) -> Result<ModelStatus, ModelManagerError> {
        validate_spec(spec)?;
        let Some(snapshot) = self.capture_existing_root()? else {
            return Ok(ModelStatus::Missing);
        };
        let physical_path = snapshot.physical_model_path(spec);
        let lock = model_lock(&physical_path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        snapshot.verify_mapping()?;
        self.inspect_path(spec, &snapshot)
    }

    fn verify_spec(&self, spec: &ModelSpec) -> Result<PathBuf, ModelManagerError> {
        match self.status_spec(spec)? {
            ModelStatus::Missing => Err(ModelManagerError::MissingModel {
                path: self.path_for_spec(spec)?,
            }),
            ModelStatus::Ready { path } => Ok(path),
            ModelStatus::Invalid { path, reason } => {
                Err(ModelManagerError::Integrity { path, reason })
            }
        }
    }

    #[cfg(test)]
    pub(super) fn ensure_spec(
        &self,
        spec: &ModelSpec,
        downloader: &dyn ModelDownloader,
    ) -> Result<PathBuf, ModelManagerError> {
        self.ensure_spec_with_cancellation(spec, downloader, &never_cancelled)
    }

    #[cfg(test)]
    pub(super) fn ensure_spec_with_cancellation(
        &self,
        spec: &ModelSpec,
        downloader: &dyn ModelDownloader,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<PathBuf, ModelManagerError> {
        let begin_commit = || check_cancelled(cancelled).is_ok();
        self.ensure_spec_with_cancellation_and_commit(spec, downloader, cancelled, &begin_commit)
    }

    pub(super) fn ensure_spec_with_cancellation_and_commit(
        &self,
        spec: &ModelSpec,
        downloader: &dyn ModelDownloader,
        cancelled: &dyn Fn() -> bool,
        begin_commit: &dyn Fn() -> bool,
    ) -> Result<PathBuf, ModelManagerError> {
        validate_spec(spec)?;
        check_cancelled(cancelled)?;
        let snapshot = self.create_and_capture_root()?;
        let physical_path = snapshot.physical_model_path(spec);
        let reported_path = snapshot.configured_model_path(spec);
        let lock = model_lock(&physical_path);
        let _guard = lock_model_with_cancellation(lock.as_ref(), cancelled)?;
        snapshot.verify_mapping()?;

        check_cancelled(cancelled)?;
        let status = self.inspect_path(spec, &snapshot);
        check_cancelled(cancelled)?;
        if matches!(status?, ModelStatus::Ready { .. }) {
            return Ok(reported_path);
        }

        let mut temporary = TemporaryDownload::create(&snapshot, spec.filename())?;
        let temporary_path = temporary.path.clone();
        check_cancelled(cancelled)?;
        let download = downloader.download(spec);
        check_cancelled(cancelled)?;
        let mut reader = download?;
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; DOWNLOAD_BUFFER_BYTES];

        loop {
            let read_length = next_read_length(spec.exact_bytes(), total, buffer.len());
            check_cancelled(cancelled)?;
            let read = reader.read(&mut buffer[..read_length]);
            check_cancelled(cancelled)?;
            let count = read.map_err(|source| ModelManagerError::Io {
                operation: "read model download",
                path: temporary_path.clone(),
                source,
            })?;
            if count == 0 {
                break;
            }
            let observed = total.saturating_add(count as u64);
            if observed > spec.exact_bytes() {
                return Err(ModelManagerError::DownloadTooLarge {
                    expected: spec.exact_bytes(),
                    observed,
                });
            }

            temporary
                .file_mut()
                .write_all(&buffer[..count])
                .map_err(|source| ModelManagerError::Io {
                    operation: "write model download",
                    path: temporary_path.clone(),
                    source,
                })?;
            hasher.update(&buffer[..count]);
            total = observed;
        }

        if total != spec.exact_bytes() {
            return Err(integrity_error(
                reported_path.clone(),
                ModelIntegrityError::WrongSize {
                    expected: spec.exact_bytes(),
                    actual: total,
                },
            ));
        }

        let actual_sha256 = digest_to_hex(&hasher.finalize());
        if actual_sha256 != spec.sha256() {
            return Err(integrity_error(
                reported_path.clone(),
                ModelIntegrityError::ChecksumMismatch {
                    expected: spec.sha256().to_owned(),
                    actual: actual_sha256,
                },
            ));
        }

        check_cancelled(cancelled)?;
        temporary
            .file_mut()
            .flush()
            .map_err(|source| ModelManagerError::Io {
                operation: "flush model download",
                path: temporary_path.clone(),
                source,
            })?;
        check_cancelled(cancelled)?;
        temporary
            .file_mut()
            .sync_all()
            .map_err(|source| ModelManagerError::Io {
                operation: "sync model download",
                path: temporary_path,
                source,
            })?;
        check_cancelled(cancelled)?;

        temporary.verify_current()?;
        let target_exists = ensure_safe_replace_target(&snapshot, spec)?;
        snapshot.verify_mapping()?;
        temporary.commit(spec.filename(), target_exists, begin_commit)?;
        sync_containing_directory(&snapshot, &physical_path)?;
        snapshot.verify_mapping()?;
        Ok(reported_path)
    }

    pub(super) fn remove_spec(&self, spec: &ModelSpec) -> Result<bool, ModelManagerError> {
        validate_spec(spec)?;
        let Some(snapshot) = self.capture_existing_root()? else {
            return Ok(false);
        };
        let physical_path = snapshot.physical_model_path(spec);
        let lock = model_lock(&physical_path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        snapshot.verify_mapping()?;
        self.remove_from_snapshot(spec, &snapshot)
    }

    pub(super) fn remove_from_snapshot(
        &self,
        spec: &ModelSpec,
        snapshot: &RootSnapshot,
    ) -> Result<bool, ModelManagerError> {
        let path = snapshot.physical_model_path(spec);
        let metadata = match snapshot.dir.symlink_metadata(spec.filename()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                snapshot.verify_mapping()?;
                return Ok(false);
            }
            Err(source) => {
                return Err(ModelManagerError::Io {
                    operation: "inspect model file",
                    path: path.clone(),
                    source,
                });
            }
        };
        ensure_cap_regular_model_file(&path, &metadata)?;
        snapshot.verify_mapping()?;
        snapshot
            .dir
            .remove_file(spec.filename())
            .map_err(|source| ModelManagerError::Io {
                operation: "remove model file",
                path: path.clone(),
                source,
            })?;
        snapshot.verify_mapping()?;
        Ok(true)
    }

    pub(super) fn path_for_spec(&self, spec: &ModelSpec) -> Result<PathBuf, ModelManagerError> {
        validate_spec(spec)?;
        Ok(self.root.join(spec.filename()))
    }

    pub(super) fn inspect_path(
        &self,
        spec: &ModelSpec,
        snapshot: &RootSnapshot,
    ) -> Result<ModelStatus, ModelManagerError> {
        let path = snapshot.physical_model_path(spec);
        let reported_path = snapshot.configured_model_path(spec);
        let metadata = match snapshot.dir.symlink_metadata(spec.filename()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                snapshot.verify_mapping()?;
                return Ok(ModelStatus::Missing);
            }
            Err(source) => {
                return Err(ModelManagerError::Io {
                    operation: "inspect model file",
                    path,
                    source,
                });
            }
        };
        ensure_cap_regular_model_file(&path, &metadata)?;
        if metadata.len() != spec.exact_bytes() {
            snapshot.verify_mapping()?;
            return Ok(ModelStatus::Invalid {
                path: reported_path,
                reason: ModelIntegrityError::WrongSize {
                    expected: spec.exact_bytes(),
                    actual: metadata.len(),
                },
            });
        }

        let mut file =
            snapshot
                .dir
                .open(spec.filename())
                .map_err(|source| ModelManagerError::Io {
                    operation: "open model file",
                    path: path.clone(),
                    source,
                })?;
        let identity_file = file
            .try_clone()
            .map(CapabilityFile::into_std)
            .map_err(|source| ModelManagerError::Io {
                operation: "clone opened model file",
                path: path.clone(),
                source,
            })?;
        let opened_metadata = identity_file
            .metadata()
            .map_err(|source| ModelManagerError::Io {
                operation: "inspect opened model file",
                path: path.clone(),
                source,
            })?;
        ensure_regular_model_file(&path, &opened_metadata)?;
        let opened_version = FileVersion::from_metadata(&opened_metadata);
        let opened_identity =
            FileIdentity::from_file(identity_file).map_err(|source| ModelManagerError::Io {
                operation: "identify opened model file",
                path: path.clone(),
                source,
            })?;

        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; DOWNLOAD_BUFFER_BYTES];
        loop {
            let read_length = next_read_length(spec.exact_bytes(), total, buffer.len());
            let count =
                file.read(&mut buffer[..read_length])
                    .map_err(|source| ModelManagerError::Io {
                        operation: "read model file",
                        path: path.clone(),
                        source,
                    })?;
            if count == 0 {
                break;
            }
            total = total.saturating_add(count as u64);
            if total > spec.exact_bytes() {
                verify_model_file_unchanged(snapshot, spec, &opened_identity, &opened_version)?;
                return Ok(ModelStatus::Invalid {
                    path: reported_path,
                    reason: ModelIntegrityError::WrongSize {
                        expected: spec.exact_bytes(),
                        actual: total,
                    },
                });
            }
            hasher.update(&buffer[..count]);
        }
        let actual_sha256 = digest_to_hex(&hasher.finalize());
        let final_metadata =
            verify_model_file_unchanged(snapshot, spec, &opened_identity, &opened_version)?;
        if total != spec.exact_bytes() {
            return Ok(ModelStatus::Invalid {
                path: reported_path,
                reason: ModelIntegrityError::WrongSize {
                    expected: spec.exact_bytes(),
                    actual: total,
                },
            });
        }

        if actual_sha256 != spec.sha256() {
            return Ok(ModelStatus::Invalid {
                path: reported_path,
                reason: ModelIntegrityError::ChecksumMismatch {
                    expected: spec.sha256().to_owned(),
                    actual: actual_sha256,
                },
            });
        }

        if final_metadata.len() != spec.exact_bytes() {
            return Ok(ModelStatus::Invalid {
                path: reported_path,
                reason: ModelIntegrityError::WrongSize {
                    expected: spec.exact_bytes(),
                    actual: final_metadata.len(),
                },
            });
        }

        snapshot.verify_mapping()?;
        Ok(ModelStatus::Ready {
            path: reported_path,
        })
    }

    pub(super) fn capture_existing_root(&self) -> Result<Option<RootSnapshot>, ModelManagerError> {
        let metadata = match fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ModelManagerError::Io {
                    operation: "inspect model root",
                    path: self.root.clone(),
                    source,
                });
            }
        };
        ensure_directory_root(&self.root, &metadata)?;
        RootSnapshot::capture(&self.root).map(Some)
    }

    fn create_and_capture_root(&self) -> Result<RootSnapshot, ModelManagerError> {
        fs::create_dir_all(&self.root).map_err(|source| ModelManagerError::Io {
            operation: "create model root",
            path: self.root.clone(),
            source,
        })?;
        RootSnapshot::capture(&self.root)
    }
}

pub(super) struct RootSnapshot {
    lexical_root: PathBuf,
    canonical_root: PathBuf,
    dir: CapabilityDir,
    identity: FileIdentity,
}

impl RootSnapshot {
    fn capture(lexical_root: &Path) -> Result<Self, ModelManagerError> {
        let metadata =
            fs::symlink_metadata(lexical_root).map_err(|source| ModelManagerError::Io {
                operation: "inspect model root",
                path: lexical_root.to_path_buf(),
                source,
            })?;
        ensure_directory_root(lexical_root, &metadata)?;

        let canonical_root =
            fs::canonicalize(lexical_root).map_err(|source| ModelManagerError::Io {
                operation: "canonicalize model root",
                path: lexical_root.to_path_buf(),
                source,
            })?;
        let dir = CapabilityDir::open_ambient_dir(&canonical_root, ambient_authority()).map_err(
            |source| ModelManagerError::Io {
                operation: "open canonical model root",
                path: canonical_root.clone(),
                source,
            },
        )?;
        let identity_file =
            dir.try_clone()
                .map(CapabilityDir::into_std_file)
                .map_err(|source| ModelManagerError::Io {
                    operation: "clone canonical model root handle",
                    path: canonical_root.clone(),
                    source,
                })?;
        let opened_metadata = identity_file
            .metadata()
            .map_err(|source| ModelManagerError::Io {
                operation: "inspect canonical model root handle",
                path: canonical_root.clone(),
                source,
            })?;
        ensure_directory_root(&canonical_root, &opened_metadata)?;
        let identity =
            FileIdentity::from_file(identity_file).map_err(|source| ModelManagerError::Io {
                operation: "identify canonical model root",
                path: canonical_root.clone(),
                source,
            })?;

        let snapshot = Self {
            lexical_root: lexical_root.to_path_buf(),
            canonical_root,
            dir,
            identity,
        };
        snapshot.verify_mapping()?;
        Ok(snapshot)
    }

    pub(super) fn physical_model_path(&self, spec: &ModelSpec) -> PathBuf {
        self.canonical_root.join(spec.filename())
    }

    fn configured_model_path(&self, spec: &ModelSpec) -> PathBuf {
        self.lexical_root.join(spec.filename())
    }

    fn verify_mapping(&self) -> Result<(), ModelManagerError> {
        let unchanged = || -> Option<bool> {
            let metadata = fs::symlink_metadata(&self.lexical_root).ok()?;
            if ensure_directory_root(&self.lexical_root, &metadata).is_err() {
                return Some(false);
            }

            let canonical = fs::canonicalize(&self.lexical_root).ok()?;
            if canonical != self.canonical_root {
                return Some(false);
            }

            let identity = FileIdentity::from_path(&self.lexical_root).ok()?;
            if identity != self.identity {
                return Some(false);
            }

            let final_metadata = fs::symlink_metadata(&self.lexical_root).ok()?;
            if ensure_directory_root(&self.lexical_root, &final_metadata).is_err() {
                return Some(false);
            }
            let final_canonical = fs::canonicalize(&self.lexical_root).ok()?;
            Some(final_canonical == self.canonical_root)
        };

        if unchanged() == Some(true) {
            Ok(())
        } else {
            Err(ModelManagerError::RootMappingChanged {
                root: self.lexical_root.clone(),
                pinned_root: self.canonical_root.clone(),
            })
        }
    }
}

fn verify_model_file_unchanged(
    snapshot: &RootSnapshot,
    spec: &ModelSpec,
    expected_identity: &FileIdentity,
    expected_version: &FileVersion,
) -> Result<cap_std::fs::Metadata, ModelManagerError> {
    let path = snapshot.physical_model_path(spec);
    let metadata = snapshot
        .dir
        .symlink_metadata(spec.filename())
        .map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                ModelManagerError::FilesystemObjectChanged {
                    path: path.clone(),
                    object: "model file",
                }
            } else {
                ModelManagerError::Io {
                    operation: "reinspect model file",
                    path: path.clone(),
                    source,
                }
            }
        })?;
    ensure_cap_regular_model_file(&path, &metadata)?;

    let current_file = snapshot
        .dir
        .open(spec.filename())
        .map(CapabilityFile::into_std)
        .map_err(|source| ModelManagerError::Io {
            operation: "reopen model file for identity verification",
            path: path.clone(),
            source,
        })?;
    let current_metadata = current_file
        .metadata()
        .map_err(|source| ModelManagerError::Io {
            operation: "reinspect opened model file",
            path: path.clone(),
            source,
        })?;
    ensure_regular_model_file(&path, &current_metadata)?;
    let current_version = FileVersion::from_metadata(&current_metadata);
    let current_identity =
        FileIdentity::from_file(current_file).map_err(|source| ModelManagerError::Io {
            operation: "reidentify model file",
            path: path.clone(),
            source,
        })?;
    if &current_identity != expected_identity || &current_version != expected_version {
        return Err(ModelManagerError::FilesystemObjectChanged {
            path,
            object: "model file",
        });
    }

    snapshot.verify_mapping()?;
    Ok(metadata)
}

fn ensure_safe_replace_target(
    snapshot: &RootSnapshot,
    spec: &ModelSpec,
) -> Result<bool, ModelManagerError> {
    let path = snapshot.physical_model_path(spec);
    let metadata = match snapshot.dir.symlink_metadata(spec.filename()) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(ModelManagerError::Io {
                operation: "inspect model replacement target",
                path,
                source,
            });
        }
    };
    ensure_cap_regular_model_file(&path, &metadata)?;
    Ok(true)
}

#[cfg(unix)]
fn sync_containing_directory(
    snapshot: &RootSnapshot,
    installed_path: &Path,
) -> Result<(), ModelManagerError> {
    // cap-std opens directories with O_PATH on Linux, and fsync on an O_PATH
    // descriptor fails with EBADF, so reopen a regular read-only descriptor
    // and confirm it still names the pinned root before syncing through it.
    let sync_failed = |source| ModelManagerError::InstalledButDirectorySyncFailed {
        path: installed_path.to_path_buf(),
        source,
    };
    let directory = fs::File::open(&snapshot.canonical_root).map_err(sync_failed)?;
    let identity = FileIdentity::from_file(directory).map_err(sync_failed)?;
    if identity != snapshot.identity {
        return Err(ModelManagerError::RootMappingChanged {
            root: snapshot.lexical_root.clone(),
            pinned_root: snapshot.canonical_root.clone(),
        });
    }
    identity.as_file().sync_all().map_err(sync_failed)
}

#[cfg(not(unix))]
fn sync_containing_directory(
    _snapshot: &RootSnapshot,
    _installed_path: &Path,
) -> Result<(), ModelManagerError> {
    Ok(())
}

struct TemporaryDownload {
    root: CapabilityDir,
    name: PathBuf,
    path: PathBuf,
    file: Option<CapabilityFile>,
    identity: Option<FileIdentity>,
    committed: bool,
}

impl TemporaryDownload {
    fn create(snapshot: &RootSnapshot, model_filename: &str) -> Result<Self, ModelManagerError> {
        let root = snapshot
            .dir
            .try_clone()
            .map_err(|source| ModelManagerError::Io {
                operation: "clone pinned model root",
                path: snapshot.canonical_root.clone(),
                source,
            })?;
        for _ in 0..TEMP_FILE_ATTEMPTS {
            let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = PathBuf::from(format!(
                ".{model_filename}.download-{}-{sequence}.tmp",
                std::process::id()
            ));
            let path = snapshot.canonical_root.join(&name);
            let mut options = CapabilityOpenOptions::new();
            options.write(true).create_new(true);
            match root.open_with(&name, &options) {
                Ok(file) => {
                    let mut temporary = Self {
                        root,
                        name,
                        path,
                        file: Some(file),
                        identity: None,
                        committed: false,
                    };
                    let identity_file = temporary
                        .file
                        .as_ref()
                        .expect("new temporary file is open")
                        .try_clone()
                        .map(CapabilityFile::into_std)
                        .map_err(|source| ModelManagerError::Io {
                            operation: "clone temporary model file",
                            path: temporary.path.clone(),
                            source,
                        })?;
                    let metadata =
                        identity_file
                            .metadata()
                            .map_err(|source| ModelManagerError::Io {
                                operation: "inspect temporary model file",
                                path: temporary.path.clone(),
                                source,
                            })?;
                    ensure_regular_model_file(&temporary.path, &metadata)?;
                    temporary.identity =
                        Some(FileIdentity::from_file(identity_file).map_err(|source| {
                            ModelManagerError::Io {
                                operation: "identify temporary model file",
                                path: temporary.path.clone(),
                                source,
                            }
                        })?);
                    return Ok(temporary);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(ModelManagerError::Io {
                        operation: "create temporary model file",
                        path,
                        source,
                    });
                }
            }
        }

        Err(ModelManagerError::TemporaryNameExhausted {
            root: snapshot.canonical_root.clone(),
        })
    }

    fn file_mut(&mut self) -> &mut CapabilityFile {
        self.file
            .as_mut()
            .expect("temporary model file is open until commit")
    }

    fn verify_current(&self) -> Result<(), ModelManagerError> {
        let metadata =
            self.root
                .symlink_metadata(&self.name)
                .map_err(|source| ModelManagerError::Io {
                    operation: "reinspect temporary model file",
                    path: self.path.clone(),
                    source,
                })?;
        ensure_cap_regular_model_file(&self.path, &metadata)?;

        let current_file = self
            .root
            .open(&self.name)
            .map(CapabilityFile::into_std)
            .map_err(|source| ModelManagerError::Io {
                operation: "reopen temporary model file",
                path: self.path.clone(),
                source,
            })?;
        let current_metadata = current_file
            .metadata()
            .map_err(|source| ModelManagerError::Io {
                operation: "reinspect opened temporary model file",
                path: self.path.clone(),
                source,
            })?;
        ensure_regular_model_file(&self.path, &current_metadata)?;
        let current_identity =
            FileIdentity::from_file(current_file).map_err(|source| ModelManagerError::Io {
                operation: "reidentify temporary model file",
                path: self.path.clone(),
                source,
            })?;
        if self.identity.as_ref() != Some(&current_identity) {
            return Err(ModelManagerError::FilesystemObjectChanged {
                path: self.path.clone(),
                object: "temporary model file",
            });
        }
        Ok(())
    }

    fn commit(
        mut self,
        target_name: &str,
        target_exists: bool,
        begin_commit: &dyn Fn() -> bool,
    ) -> Result<(), ModelManagerError> {
        drop(self.identity.take());
        drop(self.file.take());

        check_begin_commit(begin_commit)?;

        #[cfg(windows)]
        if target_exists {
            self.root
                .remove_file(target_name)
                .map_err(|source| ModelManagerError::Io {
                    operation: "remove invalid model before replacement",
                    path: self.root_path(target_name),
                    source,
                })?;
        }
        #[cfg(not(windows))]
        let _ = target_exists;

        self.root
            .rename(&self.name, &self.root, target_name)
            .map_err(|source| ModelManagerError::Io {
                operation: "atomically install model file",
                path: self.root_path(target_name),
                source,
            })?;
        self.committed = true;
        Ok(())
    }

    fn root_path(&self, name: &str) -> PathBuf {
        self.path
            .parent()
            .expect("temporary model path has a parent")
            .join(name)
    }
}

impl Drop for TemporaryDownload {
    fn drop(&mut self) {
        if !self.committed {
            drop(self.identity.take());
            drop(self.file.take());
            let _ = self.root.remove_file(&self.name);
        }
    }
}
