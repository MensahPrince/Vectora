use super::*;

/// Display name + last-modified time for one draft, for the launch gallery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSummary {
    /// Display name (from the meta sidecar, or a fallback).
    pub name: String,
    /// The draft's `project.cutlass` path — the identity used to open it.
    pub project: PathBuf,
    /// When the project file was last written (newest first in the gallery).
    pub modified: SystemTime,
}

/// Create a fresh, empty draft directory and return its `project.cutlass`
/// path (the file itself is written by the first save once the engine binds
/// to it). Ids are time-based with a per-process counter so two creates in
/// the same instant never collide.
pub fn create() -> std::io::Result<PathBuf> {
    create_in_root(&root_dir())
}

/// Copy an external `.cutlass` file into a new draft directory (Open file…),
/// naming the draft after the source file's stem. Returns the new draft's
/// `project.cutlass` path. The original file on disk is left untouched — it
/// was only an import source. A user-selected source symlink is accepted, but
/// only as a read-only boundary: its opened target must be a regular file.
pub fn import_external(source: &Path) -> std::io::Result<PathBuf> {
    import_external_in_root(&root_dir(), source)
}

/// Cache `name` in the draft's meta sidecar. Called on every auto-save so the
/// gallery name tracks the project name. Failures only log — the project file
/// is the source of truth; a stale gallery name never loses work.
pub fn write_meta(project: &Path, name: &str) {
    if let Err(error) = write_meta_in_root(&root_dir(), project, name) {
        warn!(
            project = %bounded_path(project),
            error = %error,
            "draft meta write failed"
        );
    }
}

/// Delete a draft (its whole directory), addressed by its `project.cutlass`
/// path. Missing files are fine.
pub fn delete(project: &Path) {
    if let Err(error) = delete_checked(project) {
        warn!(
            project = %bounded_path(project),
            error = %error,
            "draft deletion refused or failed"
        );
    }
}

/// Safely delete an app-owned draft.
///
/// Returns `true` once the draft has been atomically removed from the live
/// namespace. Tombstone cleanup failures are logged, but still return `true`
/// because the draft is no longer live.
pub fn delete_checked(project: &Path) -> io::Result<bool> {
    delete_checked_in_root(&root_dir(), project)
}

/// Every draft, newest-modified first — the launch gallery's source. A draft
/// directory without a readable `project.cutlass` is skipped (a half-created
/// or corrupt slot never crashes the gallery).
pub fn list() -> Vec<DraftSummary> {
    match list_in_root(&root_dir()) {
        Ok(drafts) => drafts,
        Err(error) => {
            warn!(error = %error, "draft listing failed");
            Vec::new()
        }
    }
}

/// Checked counterpart to [`list`] for callers that must distinguish an empty
/// draft store from a store that could not be read.
pub(crate) fn list_checked() -> io::Result<Vec<DraftSummary>> {
    list_in_root(&root_dir())
}

/// A coarse "5 minutes ago" label for `modified`, dependency-free, for the
/// gallery card subtitle. Future or unreadable times read as "just now".
pub fn relative_time(modified: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(modified)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86_399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86_400, "day"),
    }
}

pub(super) fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}
