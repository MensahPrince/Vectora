//! On-disk project files (`.cutlass` JSON).

use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::error::ModelError;
use crate::project::Project;
use crate::schema::{PROJECT_SCHEMA_VERSION, ProjectSchema};

/// Recommended extension for Cutlass project files.
pub const PROJECT_FILE_EXTENSION: &str = "cutlass";

/// Numeric version from [`ProjectSchema::current`] for simple callers.
pub const PROJECT_FILE_VERSION: u32 = PROJECT_SCHEMA_VERSION;

const UNIQUE_PATH_ATTEMPTS: usize = 128;
static UNIQUE_PATH_COUNTER: AtomicU64 = AtomicU64::new(0);

impl Project {
    /// Serialize this project to a `.cutlass` JSON file.
    ///
    /// The document is stamped with this build's schema version regardless
    /// of what was loaded: the writer defines the format. (A project opened
    /// from a v1 file may now hold v2-only data like keyframes; persisting
    /// it as "v1" would lie to older readers.)
    pub fn save_to_file(&self, path: &Path) -> io::Result<()> {
        let mut doc = self.clone();
        doc.schema.version = PROJECT_SCHEMA_VERSION;
        doc.schema.kind = crate::schema::PROJECT_SCHEMA_KIND.into();
        let mut json = serde_json::to_string_pretty(&doc).map_err(io::Error::other)?;
        json.push('\n');

        // Keep serialization entirely ahead of filesystem inspection so a
        // serialization failure cannot disturb an existing project.
        persist_serialized(path, json.as_bytes(), &StdPersistenceFs)
    }

    /// Deserialize a project from a `.cutlass` JSON file.
    ///
    /// The document's schema version is read and validated *before* the
    /// typed parse, and [`migrate_document`] rewrites older shapes up to the
    /// current one — so the strict parse below only ever sees the current
    /// format. Files newer than this build are refused, never half-parsed.
    pub fn load_from_file(path: &Path) -> Result<Project, ModelError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ModelError::InvalidProjectFile(e.to_string()))?;
        let mut doc: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ModelError::InvalidProjectFile(e.to_string()))?;

        unwrap_legacy_envelope(&mut doc);
        let mut schema = read_schema(&doc)?;
        normalize_legacy_schema(&mut schema);
        validate_schema(&schema)?;
        migrate_document(&mut doc, schema.version);

        let mut project: Project = serde_json::from_value(doc)
            .map_err(|e| ModelError::InvalidProjectFile(e.to_string()))?;
        // Keep the file's original (normalized) schema as provenance; the
        // writer re-stamps the current version on save.
        project.schema = schema;
        // Files written before the main-track flag (or edited externally)
        // re-derive the lane-zone / main-track invariants on entry.
        project.timeline_mut().normalize_lanes();
        Ok(project)
    }
}

trait PersistenceFs {
    fn create_new(&self, path: &Path) -> io::Result<File>;
    fn write_all(&self, file: &mut File, contents: &[u8]) -> io::Result<()>;
    fn flush(&self, file: &mut File) -> io::Result<()>;
    fn set_permissions(&self, file: &File, permissions: Permissions) -> io::Result<()>;
    fn sync_all(&self, file: &File) -> io::Result<()>;
    fn symlink_metadata(&self, path: &Path) -> io::Result<std::fs::Metadata>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn remove_dir(&self, path: &Path) -> io::Result<()>;
}

struct StdPersistenceFs;

impl PersistenceFs for StdPersistenceFs {
    fn create_new(&self, path: &Path) -> io::Result<File> {
        open_new_temp(path)
    }

    fn write_all(&self, file: &mut File, contents: &[u8]) -> io::Result<()> {
        file.write_all(contents)
    }

    fn flush(&self, file: &mut File) -> io::Result<()> {
        file.flush()
    }

    fn set_permissions(&self, file: &File, permissions: Permissions) -> io::Result<()> {
        file.set_permissions(permissions)
    }

    fn sync_all(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<std::fs::Metadata> {
        std::fs::symlink_metadata(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        create_private_dir(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir(path)
    }
}

fn persist_serialized(
    destination: &Path,
    contents: &[u8],
    fs: &impl PersistenceFs,
) -> io::Result<()> {
    let permissions = destination_permissions(destination, fs)?;
    let temporary = write_synced_temp(destination, contents, permissions, fs)?;
    install_temp_with_ops(destination, &temporary, fs)
}

fn destination_permissions(
    destination: &Path,
    fs: &impl PersistenceFs,
) -> io::Result<Option<Permissions>> {
    match fs.symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to save through a symbolic-link project destination",
        )),
        Ok(metadata) if !metadata.file_type().is_file() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "project destination is not a regular file",
        )),
        Ok(metadata) => Ok(Some(metadata.permissions())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(contextual_error(
            "could not inspect the project destination",
            error,
        )),
    }
}

fn write_synced_temp(
    destination: &Path,
    contents: &[u8],
    permissions: Option<Permissions>,
    fs: &impl PersistenceFs,
) -> io::Result<PathBuf> {
    let (temporary, mut file) = create_unique_temp(destination, fs)?;
    let result = (|| {
        fs.write_all(&mut file, contents)
            .map_err(|error| contextual_error("could not write the temporary project", error))?;
        fs.flush(&mut file)
            .map_err(|error| contextual_error("could not flush the temporary project", error))?;
        if let Some(permissions) = permissions {
            fs.set_permissions(&file, permissions).map_err(|error| {
                contextual_error("could not preserve project file permissions", error)
            })?;
        }
        fs.sync_all(&file)
            .map_err(|error| contextual_error("could not sync the temporary project", error))
    })();
    drop(file);

    match result {
        Ok(()) => Ok(temporary),
        Err(error) => Err(cleanup_temp_after_error(fs, &temporary, error)),
    }
}

fn create_unique_temp(destination: &Path, fs: &impl PersistenceFs) -> io::Result<(PathBuf, File)> {
    for _ in 0..UNIQUE_PATH_ATTEMPTS {
        let candidate = unique_sibling_path(destination, "tmp")?;
        match fs.create_new(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(contextual_error(
                    "could not create a temporary project file",
                    error,
                ));
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary project file",
    ))
}

fn open_new_temp(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[derive(Debug)]
struct BackupPaths {
    container: PathBuf,
    project: PathBuf,
}

fn create_unique_backup(destination: &Path, fs: &impl PersistenceFs) -> io::Result<BackupPaths> {
    for _ in 0..UNIQUE_PATH_ATTEMPTS {
        let container = unique_sibling_path(destination, "backup")?;
        match fs.create_dir(&container) {
            Ok(()) => {
                let project = container.join("original");
                return Ok(BackupPaths { container, project });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(contextual_error(
                    "could not create a rollback backup for the project",
                    error,
                ));
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique rollback backup for the project",
    ))
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn install_temp_with_ops(
    destination: &Path,
    temporary: &Path,
    fs: &impl PersistenceFs,
) -> io::Result<()> {
    // Recheck after the potentially long write/sync phase. This is not a
    // platform-specific openat lock, but it fails closed for every symlink
    // destination observed by the portable persistence path.
    if let Err(error) = destination_permissions(destination, fs) {
        return Err(cleanup_temp_after_error(fs, temporary, error));
    }

    let atomic_error = match fs.rename(temporary, destination) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };

    match destination_permissions(destination, fs) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let error =
                contextual_error("could not atomically install the new project", atomic_error);
            return Err(cleanup_temp_after_error(fs, temporary, error));
        }
        Err(error) => return Err(cleanup_temp_after_error(fs, temporary, error)),
    }

    // Some platforms cannot rename over an existing file. Reserve a private,
    // unique sibling directory first, then move the old file into it. The
    // original is always either at the destination or in this backup; it is
    // never deleted before the new file reaches the commit point.
    let backup = match create_unique_backup(destination, fs) {
        Ok(backup) => backup,
        Err(error) => {
            let error = io::Error::new(
                error.kind(),
                format!(
                    "atomic project replacement failed ({atomic_error}); \
                     rollback backup creation also failed: {error}"
                ),
            );
            return Err(cleanup_temp_after_error(fs, temporary, error));
        }
    };

    if let Err(backup_error) = fs.rename(destination, &backup.project) {
        let error = io::Error::new(
            backup_error.kind(),
            format!(
                "atomic project replacement failed ({atomic_error}); moving the existing project \
                 to a rollback backup also failed: {backup_error}; the existing project was left \
                 in place"
            ),
        );
        let error = cleanup_empty_backup_after_error(fs, &backup.container, error);
        return Err(cleanup_temp_after_error(fs, temporary, error));
    }

    match fs.rename(temporary, destination) {
        Ok(()) => {
            // The new destination is committed. Cleanup is deliberately
            // best-effort: a stale backup must not turn a successful save into
            // an error after callers can already observe the new project.
            let _ = fs.remove_file(&backup.project);
            let _ = fs.remove_dir(&backup.container);
            Ok(())
        }
        Err(install_error) => match fs.rename(&backup.project, destination) {
            Ok(()) => {
                let error = io::Error::new(
                    install_error.kind(),
                    format!(
                        "could not install the new project after creating a rollback backup: \
                         {install_error}; the original project was restored"
                    ),
                );
                let error = cleanup_empty_backup_after_error(fs, &backup.container, error);
                Err(cleanup_temp_after_error(fs, temporary, error))
            }
            Err(rollback_error) => {
                let error = io::Error::new(
                    install_error.kind(),
                    format!(
                        "could not install the new project after creating a rollback backup: \
                         {install_error}; restoring the original project failed: {rollback_error}; \
                         the original project was retained in a sibling backup"
                    ),
                );
                Err(cleanup_temp_after_error(fs, temporary, error))
            }
        },
    }
}

fn unique_sibling_path(destination: &Path, role: &str) -> io::Result<PathBuf> {
    let parent = destination_parent(destination)?;
    if destination.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "project destination has no file name",
        ));
    }

    let nonce = UNIQUE_PATH_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".cutlass-project-{role}-{}-{nonce}",
        std::process::id()
    )))
}

fn destination_parent(destination: &Path) -> io::Result<&Path> {
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "project destination has no parent directory",
        )
    })?;
    if parent.as_os_str().is_empty() {
        Ok(Path::new("."))
    } else {
        Ok(parent)
    }
}

fn cleanup_temp_after_error(
    fs: &impl PersistenceFs,
    temporary: &Path,
    primary_error: io::Error,
) -> io::Error {
    match fs.remove_file(temporary) {
        Ok(()) => primary_error,
        Err(error) if error.kind() == io::ErrorKind::NotFound => primary_error,
        Err(error) => io::Error::new(
            primary_error.kind(),
            format!(
                "{primary_error}; temporary project cleanup also failed: {error}; \
                 a temporary sibling may remain"
            ),
        ),
    }
}

fn cleanup_empty_backup_after_error(
    fs: &impl PersistenceFs,
    backup_container: &Path,
    primary_error: io::Error,
) -> io::Error {
    match fs.remove_dir(backup_container) {
        Ok(()) => primary_error,
        Err(error) if error.kind() == io::ErrorKind::NotFound => primary_error,
        Err(error) => io::Error::new(
            primary_error.kind(),
            format!(
                "{primary_error}; empty rollback-backup cleanup also failed: {error}; \
                 an empty sibling directory may remain"
            ),
        ),
    }
}

fn contextual_error(context: &'static str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{context}: {error}"))
}

/// Unwrap the legacy `{ "version", "project" }` envelope (early v1 saves)
/// to the bare project document, pushing the envelope version into the
/// inner schema when the project carries none (or a placeholder version 0).
fn unwrap_legacy_envelope(doc: &mut serde_json::Value) {
    let Some(obj) = doc.as_object_mut() else {
        return;
    };
    // A real project document has a schema (or its legacy alias); only the
    // envelope nests the project under "project". Unknown root fields named
    // "project"/"version" on a current file must not trip this.
    let looks_like_envelope = obj.contains_key("project")
        && obj.contains_key("version")
        && !obj.contains_key("schema")
        && !obj.contains_key("schema_version");
    if !looks_like_envelope {
        return;
    }
    let version = obj.get("version").and_then(serde_json::Value::as_u64);
    let Some(mut project) = obj.remove("project") else {
        return;
    };
    if let (Some(version), Some(inner)) = (version, project.as_object_mut()) {
        let placeholder = inner
            .get("schema")
            .and_then(|s| s.get("version"))
            .and_then(serde_json::Value::as_u64)
            == Some(0);
        let missing = !inner.contains_key("schema") && !inner.contains_key("schema_version");
        if missing || placeholder {
            inner.insert("schema".into(), serde_json::json!(version));
        }
    }
    *doc = project;
}

/// Read the document's schema (object or legacy bare-integer form) without
/// deserializing the whole project. Shared with the template loader, which
/// wraps a project document and versions in lockstep with it.
pub(crate) fn read_schema(doc: &serde_json::Value) -> Result<ProjectSchema, ModelError> {
    #[derive(Deserialize)]
    struct Holder(#[serde(deserialize_with = "crate::schema::deserialize")] ProjectSchema);

    let raw = doc
        .get("schema")
        .or_else(|| doc.get("schema_version"))
        .ok_or_else(|| ModelError::InvalidProjectFile("missing schema".into()))?;
    serde_json::from_value::<Holder>(raw.clone())
        .map(|holder| holder.0)
        .map_err(|e| ModelError::InvalidProjectFile(format!("invalid schema: {e}")))
}

/// Rewrite a raw project document from `from`'s shape to the current
/// schema's, one version step at a time, before the typed parse.
///
/// This is the format versioning policy (v1 roadmap M0):
///
/// - **Additive optional fields don't bump the version.** New fields ship
///   with `#[serde(default)]` (+ skip-if-default on save); readers of the
///   same version ignore fields they don't know — and drop them on resave.
///   That tolerance is the compatibility contract *within* a version.
/// - **Shape changes bump [`PROJECT_SCHEMA_VERSION`]** and add a
///   `migrate_vN_to_vN1` step here rewriting the previous shape into the
///   new one.
/// - **Newer documents are refused** before this runs
///   ([`ModelError::UnsupportedProjectSchema`] from `validate_schema`) —
///   guessing at a future format risks silent data loss.
///
/// The template loader reuses this on its embedded project document
/// (`.cutlasst` files version in lockstep with [`PROJECT_SCHEMA_VERSION`]).
pub(crate) fn migrate_document(doc: &mut serde_json::Value, from: u32) {
    for step in from..PROJECT_SCHEMA_VERSION {
        match step {
            1 => migrate_v1_to_v2(doc),
            // `validate_schema` bounds `from` to supported versions, so a
            // missing arm is a bug: a version bump landed without its step.
            _ => unreachable!("no migration step for schema v{step} -> v{}", step + 1),
        }
    }
}

/// v1 → v2 (M2 animatable params): transform properties gained the
/// `{"kf": [...]}` curve shape, but constants kept the bare v1 value shape,
/// so every v1 document is already a valid v2 document — nothing to
/// rewrite. The step exists to anchor the chain.
fn migrate_v1_to_v2(_doc: &mut serde_json::Value) {}

fn normalize_legacy_schema(schema: &mut ProjectSchema) {
    if schema.kind.is_empty() {
        schema.kind = crate::schema::PROJECT_SCHEMA_KIND.into();
    }
}

fn validate_schema(found: &ProjectSchema) -> Result<(), ModelError> {
    let expected = ProjectSchema::current();
    if !found.is_supported() {
        return Err(ModelError::UnsupportedProjectSchema {
            found: found.clone(),
            expected,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::clip::{Clip, Generator};
    use crate::time::{Rational, RationalTime, TimeRange};
    use crate::track::TrackKind;

    const R24: Rational = Rational::FPS_24;

    #[derive(Default)]
    struct FaultFs {
        operations: RefCell<Vec<&'static str>>,
        create_new_calls: Cell<usize>,
        create_dir_calls: Cell<usize>,
        rename_calls: Cell<usize>,
        remove_file_calls: Cell<usize>,
        temp_collisions: usize,
        backup_collisions: usize,
        fail_write: bool,
        fail_flush: bool,
        fail_permissions: bool,
        fail_sync: bool,
        failed_renames: Vec<usize>,
        failed_remove_files: Vec<usize>,
    }

    impl FaultFs {
        fn next_call(counter: &Cell<usize>) -> usize {
            let call = counter.get() + 1;
            counter.set(call);
            call
        }

        fn record(&self, operation: &'static str) {
            self.operations.borrow_mut().push(operation);
        }

        fn injected(operation: &str, call: usize) -> io::Error {
            io::Error::other(format!("injected {operation} failure #{call}"))
        }
    }

    impl PersistenceFs for FaultFs {
        fn create_new(&self, path: &Path) -> io::Result<File> {
            self.record("create_new");
            let call = Self::next_call(&self.create_new_calls);
            if call <= self.temp_collisions {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "injected temporary-path collision",
                ));
            }
            open_new_temp(path)
        }

        fn write_all(&self, file: &mut File, contents: &[u8]) -> io::Result<()> {
            self.record("write_all");
            if self.fail_write {
                return Err(Self::injected("write", 1));
            }
            Write::write_all(file, contents)
        }

        fn flush(&self, file: &mut File) -> io::Result<()> {
            self.record("flush");
            if self.fail_flush {
                return Err(Self::injected("flush", 1));
            }
            Write::flush(file)
        }

        fn set_permissions(&self, file: &File, permissions: Permissions) -> io::Result<()> {
            self.record("set_permissions");
            if self.fail_permissions {
                return Err(Self::injected("permission", 1));
            }
            file.set_permissions(permissions)
        }

        fn sync_all(&self, file: &File) -> io::Result<()> {
            self.record("sync_all");
            if self.fail_sync {
                return Err(Self::injected("sync", 1));
            }
            file.sync_all()
        }

        fn symlink_metadata(&self, path: &Path) -> io::Result<std::fs::Metadata> {
            std::fs::symlink_metadata(path)
        }

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.record("rename");
            let call = Self::next_call(&self.rename_calls);
            if self.failed_renames.contains(&call) {
                return Err(Self::injected("rename", call));
            }
            std::fs::rename(from, to)
        }

        fn create_dir(&self, path: &Path) -> io::Result<()> {
            self.record("create_dir");
            let call = Self::next_call(&self.create_dir_calls);
            if call <= self.backup_collisions {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "injected backup-path collision",
                ));
            }
            create_private_dir(path)
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            self.record("remove_file");
            let call = Self::next_call(&self.remove_file_calls);
            if self.failed_remove_files.contains(&call) {
                return Err(Self::injected("remove-file", call));
            }
            std::fs::remove_file(path)
        }

        fn remove_dir(&self, path: &Path) -> io::Result<()> {
            self.record("remove_dir");
            std::fs::remove_dir(path)
        }
    }

    fn transaction_artifacts(directory: &Path) -> Vec<PathBuf> {
        let mut artifacts: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with(".cutlass-project-tmp-")
                        || name.starts_with(".cutlass-project-backup-")
                })
            })
            .collect();
        artifacts.sort();
        artifacts
    }

    fn assert_no_transaction_artifacts(directory: &Path) {
        let artifacts = transaction_artifacts(directory);
        assert!(
            artifacts.is_empty(),
            "unexpected project-save artifacts: {artifacts:?}"
        );
    }

    #[test]
    fn roundtrip_save_load_preserves_timeline_and_metadata() {
        let mut project = Project::new("demo", R24);
        project.metadata_mut().description = "rough cut".into();
        project.metadata_mut().author = Some("editor".into());
        let media_id = project.add_media(crate::MediaSource::new(
            "/tmp/clip.mp4",
            1920,
            1080,
            R24,
            240,
            true,
        ));
        let track = project.add_track(TrackKind::Video, "V1");
        project
            .add_clip(
                track,
                media_id,
                TimeRange::at_rate(0, 48, R24),
                RationalTime::new(0, R24),
            )
            .unwrap();
        let overlay = project.add_track(TrackKind::Text, "T1");
        project
            .timeline_mut()
            .add_clip(
                overlay,
                Clip::generated(Generator::text("hi"), TimeRange::at_rate(48, 24, R24)),
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.cutlass");
        project.save_to_file(&path).unwrap();
        let loaded = Project::load_from_file(&path).unwrap();

        assert_eq!(loaded.schema, ProjectSchema::current());
        assert_eq!(loaded.id, project.id);
        assert_eq!(loaded.name, "demo");
        assert_eq!(loaded.metadata, project.metadata);
        assert_eq!(loaded.media_count(), 1);
        assert_eq!(loaded.timeline().clip_count(), 2);
        assert_no_transaction_artifacts(dir.path());
    }

    #[test]
    fn save_preserves_pretty_json_with_trailing_newline() {
        let project = Project::new("format", R24);
        let mut expected_document = project.clone();
        expected_document.schema.version = PROJECT_SCHEMA_VERSION;
        expected_document.schema.kind = crate::schema::PROJECT_SCHEMA_KIND.into();
        let mut expected = serde_json::to_string_pretty(&expected_document).unwrap();
        expected.push('\n');

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("format.cutlass");
        project.save_to_file(&path).unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
        assert_no_transaction_artifacts(dir.path());
    }

    #[test]
    fn atomic_save_replaces_and_roundtrips_without_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replace.cutlass");
        Project::new("old", R24).save_to_file(&path).unwrap();

        let mut replacement = Project::new("new", R24);
        replacement.metadata_mut().description = "replacement".into();
        replacement.save_to_file(&path).unwrap();

        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.name, "new");
        assert_eq!(loaded.metadata.description, "replacement");
        assert_no_transaction_artifacts(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn serialization_failure_precedes_destination_access() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.cutlass");
        let original = b"known-good project";
        std::fs::write(&path, original).unwrap();

        let mut project = Project::new("invalid path", R24);
        project.add_media(crate::MediaSource::new(
            PathBuf::from(OsString::from_vec(vec![0xff])),
            1920,
            1080,
            R24,
            24,
            false,
        ));

        project.save_to_file(&path).unwrap_err();

        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_no_transaction_artifacts(dir.path());
    }

    #[test]
    fn sync_failure_keeps_old_project_and_prevents_install() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.cutlass");
        let original = b"known-good project";
        std::fs::write(&path, original).unwrap();
        let fs = FaultFs {
            fail_sync: true,
            ..FaultFs::default()
        };

        let error = persist_serialized(&path, b"replacement", &fs).unwrap_err();

        assert!(error.to_string().contains("could not sync"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            *fs.operations.borrow(),
            [
                "create_new",
                "write_all",
                "flush",
                "set_permissions",
                "sync_all",
                "remove_file"
            ]
        );
        assert_eq!(fs.rename_calls.get(), 0);
        assert_no_transaction_artifacts(dir.path());
    }

    #[test]
    fn fallback_swap_replaces_existing_project_and_cleans_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fallback.cutlass");
        let original = b"old project";
        let replacement = b"new project";
        std::fs::write(&path, original).unwrap();
        let fs = FaultFs {
            failed_renames: vec![1],
            ..FaultFs::default()
        };

        persist_serialized(&path, replacement, &fs).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), replacement);
        assert_eq!(fs.rename_calls.get(), 3);
        assert_no_transaction_artifacts(dir.path());
    }

    #[test]
    fn fallback_install_failure_restores_original_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollback.cutlass");
        let original = b"old project";
        std::fs::write(&path, original).unwrap();
        let fs = FaultFs {
            failed_renames: vec![1, 3],
            ..FaultFs::default()
        };

        let error = persist_serialized(&path, b"new project", &fs).unwrap_err();

        assert!(
            error.to_string().contains("original project was restored"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(fs.rename_calls.get(), 4);
        assert_no_transaction_artifacts(dir.path());
    }

    #[test]
    fn failed_rollback_retains_the_only_good_copy_in_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("retained.cutlass");
        let original = b"old project";
        std::fs::write(&path, original).unwrap();
        let fs = FaultFs {
            failed_renames: vec![1, 3, 4],
            ..FaultFs::default()
        };

        let error = persist_serialized(&path, b"new project", &fs).unwrap_err();

        assert!(
            error.to_string().contains("retained in a sibling backup"),
            "{error}"
        );
        assert!(!path.exists());
        let artifacts = transaction_artifacts(dir.path());
        assert_eq!(artifacts.len(), 1, "{artifacts:?}");
        assert_eq!(
            std::fs::read(artifacts[0].join("original")).unwrap(),
            original
        );
    }

    #[test]
    fn committed_save_ignores_backup_cleanup_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("committed.cutlass");
        let original = b"old project";
        let replacement = b"new project";
        std::fs::write(&path, original).unwrap();
        let fs = FaultFs {
            failed_renames: vec![1],
            failed_remove_files: vec![1],
            ..FaultFs::default()
        };

        persist_serialized(&path, replacement, &fs).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), replacement);
        let artifacts = transaction_artifacts(dir.path());
        assert_eq!(artifacts.len(), 1, "{artifacts:?}");
        assert_eq!(
            std::fs::read(artifacts[0].join("original")).unwrap(),
            original
        );
        std::fs::remove_dir_all(&artifacts[0]).unwrap();
    }

    #[test]
    fn unique_path_allocation_is_bounded_under_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collision.cutlass");

        let temp_fs = FaultFs {
            temp_collisions: UNIQUE_PATH_ATTEMPTS,
            ..FaultFs::default()
        };
        let error = create_unique_temp(&path, &temp_fs).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(temp_fs.create_new_calls.get(), UNIQUE_PATH_ATTEMPTS);

        let backup_fs = FaultFs {
            backup_collisions: UNIQUE_PATH_ATTEMPTS,
            ..FaultFs::default()
        };
        let error = create_unique_backup(&path, &backup_fs).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(backup_fs.create_dir_calls.get(), UNIQUE_PATH_ATTEMPTS);
        assert_no_transaction_artifacts(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.cutlass");
        std::fs::write(&path, b"old project").unwrap();
        std::fs::set_permissions(&path, Permissions::from_mode(0o640)).unwrap();

        Project::new("permissions", R24)
            .save_to_file(&path)
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        assert_no_transaction_artifacts(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn save_rejects_symlink_destination_without_touching_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.cutlass");
        let destination = dir.path().join("link.cutlass");
        let original = b"known-good project";
        std::fs::write(&target, original).unwrap();
        symlink(&target, &destination).unwrap();

        let error = Project::new("replacement", R24)
            .save_to_file(&destination)
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            !error
                .to_string()
                .contains(dir.path().to_string_lossy().as_ref()),
            "error exposed the project directory: {error}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), original);
        assert!(
            std::fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_no_transaction_artifacts(dir.path());
    }

    #[test]
    fn saved_file_writes_schema_object() {
        let project = Project::new("shape", R24);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shape.cutlass");
        project.save_to_file(&path).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let schema = json.get("schema").expect("schema object");
        assert_eq!(schema["version"], PROJECT_SCHEMA_VERSION);
        assert_eq!(schema["kind"], "cutlass.project");
    }

    #[test]
    fn load_accepts_v1_schema_files() {
        // Every pre-M2 alpha save is a v1 file; v2 readers open them as-is.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.cutlass");
        std::fs::write(
            &path,
            r#"{"schema":{"version":1,"kind":"cutlass.project"},"id":1,"name":"v1","metadata":{},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]}}"#,
        )
        .unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.schema.version, 1);

        // Re-saving a v1 project writes the current format version.
        let resaved = dir.path().join("resaved.cutlass");
        loaded.save_to_file(&resaved).unwrap();
        let reloaded = Project::load_from_file(&resaved).unwrap();
        assert_eq!(reloaded.schema.version, PROJECT_SCHEMA_VERSION);
    }

    #[test]
    fn keyframed_transform_survives_save_load() {
        use crate::clip::{ClipParam, ParamValue};
        use crate::param::Easing;

        let mut project = Project::new("anim", R24);
        let track = project.add_track(TrackKind::Text, "T1");
        let clip = project
            .timeline_mut()
            .add_clip(
                track,
                Clip::generated(Generator::text("fade"), TimeRange::at_rate(0, 48, R24)),
            )
            .unwrap();
        project
            .set_param_keyframe(
                clip,
                ClipParam::Opacity,
                RationalTime::new(0, R24),
                ParamValue::Scalar(0.0),
                Easing::EaseInOut,
            )
            .unwrap();
        project
            .set_param_keyframe(
                clip,
                ClipParam::Opacity,
                RationalTime::new(24, R24),
                ParamValue::Scalar(1.0),
                Easing::Linear,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anim.cutlass");
        project.save_to_file(&path).unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        let transform = &loaded.clip(clip).unwrap().transform;
        assert!(transform.is_animated());
        assert_eq!(transform.opacity.keyframes().len(), 2);
        assert_eq!(transform.sample(24).opacity, 1.0);
    }

    #[test]
    fn load_rejects_unknown_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.cutlass");
        std::fs::write(
            &path,
            r#"{"schema":{"version":99,"kind":"cutlass.project"},"id":1,"name":"x","metadata":{},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]}}"#,
        )
        .unwrap();
        let err = Project::load_from_file(&path).unwrap_err();
        assert!(matches!(err, ModelError::UnsupportedProjectSchema { .. }));
    }

    #[test]
    fn load_accepts_legacy_integer_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-int.cutlass");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"id":1,"name":"legacy","metadata":{},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]}}"#,
        )
        .unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.schema.version, 1);
        assert_eq!(loaded.schema.kind, crate::schema::PROJECT_SCHEMA_KIND);
    }

    #[test]
    fn load_tolerates_unknown_optional_fields() {
        // The versioning policy: additive optional fields don't bump the
        // schema version, so a same-version file written by a newer build
        // may carry fields this build doesn't know. Loading must succeed
        // and keep everything this build *does* know.
        let mut project = Project::new("tolerant", R24);
        let media_id = project.add_media(crate::MediaSource::new(
            "/tmp/clip.mp4",
            1920,
            1080,
            R24,
            240,
            true,
        ));
        let track = project.add_track(TrackKind::Video, "V1");
        project
            .add_clip(
                track,
                media_id,
                TimeRange::at_rate(0, 48, R24),
                RationalTime::new(0, R24),
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tolerant.cutlass");
        project.save_to_file(&path).unwrap();

        // Doctor the saved JSON with unknown fields at every level a future
        // build is likely to extend.
        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["color_management"] = serde_json::json!({ "working_space": "bt709" });
        // The media pool serializes as `[id, source]` pairs.
        doc["media"][0][1]["pixel_aspect"] = serde_json::json!(1.0);
        doc["timeline"]["markers_future"] = serde_json::json!([{ "tick": 5, "name": "beat" }]);
        doc["timeline"]["tracks"][0][1]["clips"][0][1]["future_field"] = serde_json::json!(true);
        std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();

        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.name, "tolerant");
        assert_eq!(loaded.media_count(), 1);
        assert_eq!(loaded.timeline().clip_count(), 1);

        // The other half of the contract: unknown fields are ignored, not
        // preserved — a resave by this build drops them.
        let resaved = dir.path().join("resaved.cutlass");
        loaded.save_to_file(&resaved).unwrap();
        let json = std::fs::read_to_string(&resaved).unwrap();
        assert!(!json.contains("color_management"));
        assert!(!json.contains("markers_future"));
    }

    #[test]
    fn migration_chain_covers_every_supported_version() {
        // Walking from the oldest supported version must find a step arm
        // for every gap — `migrate_document` panics if a schema bump landed
        // without its migration step.
        for from in 1..=PROJECT_SCHEMA_VERSION {
            let mut doc = serde_json::json!({});
            migrate_document(&mut doc, from);
            // No current step rewrites anything (v1 shapes are valid v2).
            assert_eq!(doc, serde_json::json!({}));
        }
    }

    #[test]
    fn unknown_root_project_field_does_not_trip_envelope_unwrap() {
        // A current-version file may carry unknown root fields named
        // "project"/"version" (tolerance policy); the legacy-envelope
        // detection must not unwrap those.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notenvelope.cutlass");
        std::fs::write(
            &path,
            r#"{"schema":{"version":2,"kind":"cutlass.project"},"id":7,"name":"keep","metadata":{},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]},"project":"unknown-extension","version":9}"#,
        )
        .unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.name, "keep");
        assert_eq!(loaded.schema.version, 2);
    }

    #[test]
    fn load_accepts_legacy_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.cutlass");
        std::fs::write(
            &path,
            r#"{"version":1,"project":{"schema":{"version":1,"kind":"cutlass.project"},"id":1,"name":"legacy","metadata":{"description":"old envelope"},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]}}}"#,
        )
        .unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.name, "legacy");
        assert_eq!(loaded.metadata.description, "old envelope");
    }
}
