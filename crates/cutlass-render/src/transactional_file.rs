//! Transactional publication for native file exports.
//!
//! Native encoders are allowed to remove or truncate the path they open. This
//! guard gives them a uniquely owned sibling directory and only moves the
//! completed regular file to the requested destination after it has been
//! finalized and synchronized.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const STAGING_DIRECTORY_PREFIX: &str = ".cutlass-export-";
const MAX_ALLOCATION_ATTEMPTS: usize = 64;
const MAX_EXTENSION_UNITS: usize = 64;
const MAX_GENERATED_DIRECTORY_BYTES: usize = 80;

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

/// A same-filesystem staging file that atomically replaces its destination
/// only when [`TransactionalFile::publish`] succeeds.
#[derive(Debug)]
pub(crate) struct TransactionalFile {
    destination: PathBuf,
    parent: PathBuf,
    staging_directory: PathBuf,
    staging_file: PathBuf,
    cleanup_needed: bool,
}

impl TransactionalFile {
    /// Allocate a unique sibling staging directory and an empty file whose
    /// extension matches `destination`.
    pub(crate) fn new(destination: &Path) -> io::Result<Self> {
        validate_destination_path(destination)?;
        let destination = absolute_path(destination)?;
        validate_destination_shape(&destination)?;

        let parent = destination
            .parent()
            .ok_or_else(|| invalid_input("export destination has no parent directory"))?
            .to_owned();
        let staging_file_name = staging_file_name(&destination)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|error| error.duration())
            .as_nanos();

        for _ in 0..MAX_ALLOCATION_ATTEMPTS {
            let sequence = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
            let directory_name = format!(
                "{STAGING_DIRECTORY_PREFIX}{:08x}-{timestamp:032x}-{sequence:016x}",
                std::process::id()
            );
            debug_assert!(directory_name.len() <= MAX_GENERATED_DIRECTORY_BYTES);
            let staging_directory = parent.join(directory_name);

            match fs::create_dir(&staging_directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }

            let staging_file = staging_directory.join(&staging_file_name);
            let guard = Self {
                destination: destination.clone(),
                parent: parent.clone(),
                staging_directory,
                staging_file,
                cleanup_needed: true,
            };

            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&guard.staging_file)
            {
                Ok(file) => {
                    drop(file);
                    return Ok(guard);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    drop(guard);
                }
                Err(error) => {
                    drop(guard);
                    return Err(error);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique export staging directory",
        ))
    }

    pub(crate) fn staging_path(&self) -> &Path {
        &self.staging_file
    }

    /// Synchronize and atomically move the staged regular file into place.
    ///
    /// Once the atomic replacement commits, later directory-sync or staging
    /// cleanup errors are warnings rather than ordinary failures: returning an
    /// error at that point would incorrectly tell callers that publication did
    /// not happen.
    pub(crate) fn publish(mut self) -> io::Result<()> {
        validate_destination_shape(&self.destination)?;

        // Inspect the directory entry itself before opening it. In particular,
        // never follow a symlink left at the encoder's output path.
        let metadata = fs::symlink_metadata(&self.staging_file)?;
        if !metadata.file_type().is_file() {
            return Err(invalid_input(
                "export staging path is not a regular file after encoding",
            ));
        }

        let staged_file = File::open(&self.staging_file)?;
        if !staged_file.metadata()?.is_file() {
            return Err(invalid_input(
                "opened export staging artifact is not a regular file",
            ));
        }
        staged_file.sync_all()?;
        drop(staged_file);

        // The source and destination entries live in different directories,
        // so sync the source directory before moving the durable file.
        sync_directory_if_supported(&self.staging_directory)?;

        let staged_path = tempfile::TempPath::try_from_path(self.staging_file.clone())?;
        if let Err(error) = staged_path.persist(&self.destination) {
            return Err(error.error);
        }

        // The destination now names the complete file. Everything after this
        // point is best-effort housekeeping/durability reporting.
        if let Err(error) = self.cleanup() {
            tracing::warn!(
                error_kind = ?error.kind(),
                raw_os_error = ?error.raw_os_error(),
                "export committed but its staging directory could not be removed"
            );
        }
        if let Err(error) = sync_directory_if_supported(&self.parent) {
            tracing::warn!(
                error_kind = ?error.kind(),
                raw_os_error = ?error.raw_os_error(),
                "export committed but its destination directory could not be synchronized"
            );
        }

        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if !self.cleanup_needed {
            return Ok(());
        }
        match fs::remove_dir_all(&self.staging_directory) {
            Ok(()) => {
                self.cleanup_needed = false;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.cleanup_needed = false;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for TransactionalFile {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            tracing::warn!(
                error_kind = ?error.kind(),
                raw_os_error = ?error.raw_os_error(),
                "export staging cleanup failed"
            );
        }
    }
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn validate_destination_path(destination: &Path) -> io::Result<()> {
    let file_name = destination
        .file_name()
        .ok_or_else(|| invalid_input("export destination has no file name"))?;
    let mut components = Path::new(file_name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(invalid_input(
            "export destination file name is not a single path component",
        ));
    }
    Ok(())
}

fn validate_destination_shape(destination: &Path) -> io::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(invalid_input(
            "export destination exists but is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn staging_file_name(destination: &Path) -> io::Result<OsString> {
    let mut name = OsString::from("export");
    if let Some(extension) = destination.extension() {
        if os_str_units(extension) > MAX_EXTENSION_UNITS {
            return Err(invalid_input("export destination extension is too long"));
        }
        name.push(".");
        name.push(extension);
    }

    let mut components = Path::new(&name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(invalid_input(
            "export destination extension is not a safe path component",
        ));
    }
    Ok(name)
}

#[cfg(unix)]
fn os_str_units(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().len()
}

#[cfg(windows)]
fn os_str_units(value: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().count()
}

#[cfg(not(any(unix, windows)))]
fn os_str_units(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

#[cfg(unix)]
fn sync_directory_if_supported(path: &Path) -> io::Result<()> {
    match File::open(path).and_then(|directory| directory.sync_all()) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}

#[cfg(not(unix))]
fn sync_directory_if_supported(path: &Path) -> io::Result<()> {
    let _ = path;
    Ok(())
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_no_staging_directories(parent: &Path) {
        let leaked: Vec<_> = fs::read_dir(parent)
            .expect("read parent")
            .map(|entry| entry.expect("directory entry").file_name())
            .filter(|name| name.to_string_lossy().starts_with(STAGING_DIRECTORY_PREFIX))
            .collect();
        assert!(leaked.is_empty(), "leaked staging directories: {leaked:?}");
    }

    #[test]
    fn abandoned_and_unwound_staging_preserve_old_destination() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("movie.mp4");
        let sibling = root.path().join("keep.txt");
        fs::write(&destination, b"old export").expect("seed destination");
        fs::write(&sibling, b"unrelated").expect("seed sibling");

        let staged = TransactionalFile::new(&destination).expect("allocate staging");
        fs::write(staged.staging_path(), b"incomplete export").expect("write staging");
        drop(staged);

        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"old export"
        );
        assert_eq!(fs::read(&sibling).expect("read sibling"), b"unrelated");
        assert_no_staging_directories(root.path());

        let unwind = std::panic::catch_unwind(|| {
            let staged = TransactionalFile::new(&destination).expect("allocate staging");
            fs::write(staged.staging_path(), b"panicked export").expect("write staging");
            panic!("simulate encoder panic");
        });
        assert!(unwind.is_err());
        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"old export"
        );
        assert_eq!(fs::read(&sibling).expect("read sibling"), b"unrelated");
        assert_no_staging_directories(root.path());
    }

    #[test]
    fn abandoned_staging_leaves_absent_destination_absent() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("movie.mp4");

        let staged = TransactionalFile::new(&destination).expect("allocate staging");
        fs::write(staged.staging_path(), b"incomplete export").expect("write staging");
        drop(staged);

        assert!(!destination.exists());
        assert_no_staging_directories(root.path());
    }

    #[test]
    fn successful_publication_replaces_old_file_and_cleans_staging() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("movie.mp4");
        let sibling = root.path().join("keep.txt");
        fs::write(&destination, b"old export").expect("seed destination");
        fs::write(&sibling, b"unrelated").expect("seed sibling");

        let staged = TransactionalFile::new(&destination).expect("allocate staging");
        let staging_directory = staged.staging_directory.clone();
        fs::write(staged.staging_path(), b"complete export").expect("write staging");
        staged.publish().expect("publish");

        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"complete export"
        );
        assert_eq!(fs::read(&sibling).expect("read sibling"), b"unrelated");
        assert!(!staging_directory.exists());
        assert_no_staging_directories(root.path());
    }

    #[test]
    fn destination_directory_during_publication_is_preserved() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("movie.mp4");
        let staged = TransactionalFile::new(&destination).expect("allocate staging");
        fs::write(staged.staging_path(), b"complete export").expect("write staging");

        fs::create_dir(&destination).expect("create destination directory");
        fs::write(destination.join("marker"), b"keep").expect("seed destination directory");
        let error = staged
            .publish()
            .expect_err("directory must reject publication");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            fs::read(destination.join("marker")).expect("read marker"),
            b"keep"
        );
        assert_no_staging_directories(root.path());
    }

    #[test]
    fn staging_is_a_sibling_and_preserves_the_extension() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("cut.final.MP4");
        let staged = TransactionalFile::new(&destination).expect("allocate staging");

        assert_eq!(
            staged.staging_directory.parent(),
            destination.parent(),
            "staging directory must use the exact destination parent"
        );
        assert_eq!(
            staged.staging_file.parent(),
            Some(staged.staging_directory.as_path())
        );
        assert_eq!(staged.staging_file.extension(), destination.extension());
        assert!(
            staged
                .staging_directory
                .file_name()
                .expect("staging name")
                .to_string_lossy()
                .len()
                <= MAX_GENERATED_DIRECTORY_BYTES
        );

        drop(staged);
        assert_no_staging_directories(root.path());
    }

    #[cfg(unix)]
    #[test]
    fn staging_symlink_is_rejected_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("movie.mp4");
        let outside = root.path().join("outside.mp4");
        fs::write(&destination, b"old export").expect("seed destination");
        fs::write(&outside, b"outside").expect("seed outside");

        let staged = TransactionalFile::new(&destination).expect("allocate staging");
        fs::remove_file(staged.staging_path()).expect("remove placeholder");
        symlink(&outside, staged.staging_path()).expect("replace staging with symlink");
        let error = staged.publish().expect_err("symlink must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"old export"
        );
        assert_eq!(fs::read(&outside).expect("read outside"), b"outside");
        assert_no_staging_directories(root.path());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_destination_name_does_not_require_utf8_conversion() {
        use std::os::unix::ffi::OsStringExt;

        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join(OsString::from_vec(vec![
            b'v', 0xff, b'i', b'd', b'.', b'm', b'p', b'4',
        ]));
        let staged = TransactionalFile::new(&destination).expect("allocate staging");
        assert_eq!(staged.staging_file.extension(), Some(OsStr::new("mp4")));
        fs::write(staged.staging_path(), b"complete export").expect("write staging");
        drop(staged);

        assert!(!destination.exists());
        assert_no_staging_directories(root.path());
    }

    #[cfg(windows)]
    #[test]
    fn windows_publication_replaces_an_existing_file() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("movie.mp4");
        fs::write(&destination, b"old export").expect("seed destination");

        let staged = TransactionalFile::new(&destination).expect("allocate staging");
        fs::write(staged.staging_path(), b"complete export").expect("write staging");
        staged.publish().expect("replace existing destination");

        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"complete export"
        );
        assert_no_staging_directories(root.path());
    }
}
