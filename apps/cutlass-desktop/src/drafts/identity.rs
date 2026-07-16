use super::*;

/// File name of the project inside a draft directory.
pub(super) const PROJECT_FILE: &str = "project.cutlass";
/// File name of the display-name sidecar inside a draft directory.
pub(super) const META_FILE: &str = "meta.json";
/// Prefix for a draft directory after deletion has been committed.
pub(super) const TOMBSTONE_PREFIX: &str = ".cutlass-delete-";
/// The two `new_id` groups are a `u128` timestamp and a `u64` counter.
pub(super) const ID_TIME_HEX_MAX: usize = 32;
pub(super) const ID_SEQUENCE_HEX_MAX: usize = 16;
pub(super) const MAX_ERROR_PATH_CHARS: usize = 240;
pub(super) const UNIQUE_PATH_ATTEMPTS: usize = 128;

/// `projects/` in the per-user OS data dir (see [`crate::paths`]):
/// `%APPDATA%\Cutlass` on Windows, `~/Library/Application Support/Cutlass` on
/// macOS, `~/.local/share/Cutlass` on Linux.
pub fn root_dir() -> PathBuf {
    crate::paths::data_dir().join("projects")
}

/// The project file inside a draft directory.
pub fn project_file(dir: &Path) -> PathBuf {
    dir.join(PROJECT_FILE)
}

/// Resolve a canonical draft id to its app-owned lexical project path.
///
/// The draft directory and project file must already exist as real directory
/// and regular-file entries. Filesystem paths are never accepted as ids.
pub(crate) fn resolve_draft_id(draft_id: &str) -> io::Result<PathBuf> {
    resolve_draft_id_in_root(&root_dir(), draft_id)
}

/// Extract the canonical id from an exact app-owned lexical project path.
///
/// This validates identity only: neither the draft directory nor project file
/// needs to exist. That keeps newly created, not-yet-saved sessions addressable.
pub(crate) fn draft_id_from_project(project: &Path) -> io::Result<String> {
    draft_id_from_project_in_root(&root_dir(), project)
}

pub(super) fn meta_file(dir: &Path) -> PathBuf {
    dir.join(META_FILE)
}

#[derive(Debug)]
pub(super) struct DraftIdentity {
    pub(super) id: String,
}

pub(super) fn resolve_draft_id_in_root(root: &Path, draft_id: &str) -> io::Result<PathBuf> {
    if !is_valid_draft_id(draft_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "draft id must be canonical lowercase hexadecimal time-sequence",
        ));
    }

    let project = root.join(draft_id).join(PROJECT_FILE);
    let identity = validate_project_identity(root, &project)?;
    let canonical_root = canonical_existing_root(root)?
        .ok_or_else(|| path_error(io::ErrorKind::NotFound, "draft root does not exist", root))?;
    let Some(canonical_dir) = canonical_draft_dir(&canonical_root, &identity.id)? else {
        return Err(path_error(
            io::ErrorKind::NotFound,
            "draft directory does not exist",
            &project,
        ));
    };
    if !safe_project_entry(&canonical_dir)? {
        return Err(path_error(
            io::ErrorKind::NotFound,
            "draft project file does not exist",
            &project,
        ));
    }

    verify_root_mapping(root, &canonical_root)?;
    let Some(current_dir) = canonical_draft_dir(&canonical_root, &identity.id)? else {
        return Err(path_error(
            io::ErrorKind::NotFound,
            "draft directory disappeared during resolution",
            &project,
        ));
    };
    if current_dir != canonical_dir {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft directory changed during resolution",
            &project,
        ));
    }
    if !safe_project_entry(&current_dir)? {
        return Err(path_error(
            io::ErrorKind::NotFound,
            "draft project file disappeared during resolution",
            &project,
        ));
    }
    verify_root_mapping(root, &canonical_root)?;
    Ok(project)
}

pub(super) fn draft_id_from_project_in_root(root: &Path, project: &Path) -> io::Result<String> {
    validate_project_identity(root, project).map(|identity| identity.id)
}

pub(super) fn validate_project_identity(root: &Path, project: &Path) -> io::Result<DraftIdentity> {
    if project.file_name() != Some(std::ffi::OsStr::new(PROJECT_FILE)) {
        return Err(invalid_project_path(
            project,
            "filename must be exactly project.cutlass",
        ));
    }
    let Some(dir) = project.parent() else {
        return Err(invalid_project_path(
            project,
            "project path has no draft directory",
        ));
    };
    let Some(id_os) = dir.file_name() else {
        return Err(invalid_project_path(
            project,
            "project path has no draft id",
        ));
    };
    let Some(id) = id_os.to_str() else {
        return Err(invalid_project_path(project, "draft id is not valid UTF-8"));
    };
    if !is_valid_draft_id(id) {
        return Err(invalid_project_path(
            project,
            "draft id must be canonical lowercase hexadecimal time-sequence",
        ));
    }

    let expected = root.join(id).join(PROJECT_FILE);
    if project.as_os_str() != expected.as_os_str() {
        return Err(invalid_project_path(
            project,
            "path must be exactly root/draft-id/project.cutlass",
        ));
    }
    Ok(DraftIdentity { id: id.to_owned() })
}

pub(super) fn is_valid_draft_id(id: &str) -> bool {
    if id.len() < 3 || id.len() > ID_TIME_HEX_MAX + ID_SEQUENCE_HEX_MAX + 1 {
        return false;
    }
    let Some((time, sequence)) = id.split_once('-') else {
        return false;
    };
    !sequence.contains('-')
        && is_canonical_hex_group(time, ID_TIME_HEX_MAX)
        && is_canonical_hex_group(sequence, ID_SEQUENCE_HEX_MAX)
}

pub(super) fn is_canonical_hex_group(group: &str, max_len: usize) -> bool {
    !group.is_empty()
        && group.len() <= max_len
        && (group.len() == 1 || !group.starts_with('0'))
        && group
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn ensure_root(root: &Path) -> io::Result<PathBuf> {
    if let Some(canonical_root) = canonical_existing_root(root)? {
        return Ok(canonical_root);
    }
    fs::create_dir_all(root).map_err(|error| contextual_error(error, "create draft root", root))?;
    canonical_existing_root(root)?.ok_or_else(|| {
        path_error(
            io::ErrorKind::NotFound,
            "draft root disappeared after creation",
            root,
        )
    })
}

pub(super) fn canonical_existing_root(root: &Path) -> io::Result<Option<PathBuf>> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(contextual_error(error, "inspect draft root", root)),
    };
    if metadata.file_type().is_symlink() {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft root must not be a symlink",
            root,
        ));
    }
    if !metadata.is_dir() {
        return Err(path_error(
            io::ErrorKind::InvalidInput,
            "draft root is not a directory",
            root,
        ));
    }

    let canonical_root = fs::canonicalize(root)
        .map_err(|error| contextual_error(error, "canonicalize draft root", root))?;
    let canonical_metadata = fs::symlink_metadata(&canonical_root).map_err(|error| {
        contextual_error(error, "inspect canonical draft root", &canonical_root)
    })?;
    if canonical_metadata.file_type().is_symlink() || !canonical_metadata.is_dir() {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "canonical draft root is not a real directory",
            &canonical_root,
        ));
    }
    Ok(Some(canonical_root))
}

pub(super) fn verify_root_mapping(root: &Path, expected: &Path) -> io::Result<()> {
    match canonical_existing_root(root)? {
        Some(current) if current == expected => Ok(()),
        Some(_) => Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft root changed during the operation",
            root,
        )),
        None => Err(path_error(
            io::ErrorKind::NotFound,
            "draft root disappeared during the operation",
            root,
        )),
    }
}

pub(super) fn canonical_draft_dir(canonical_root: &Path, id: &str) -> io::Result<Option<PathBuf>> {
    let dir = canonical_root.join(id);
    let metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(contextual_error(error, "inspect draft directory", &dir)),
    };
    if metadata.file_type().is_symlink() {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft directory must not be a symlink",
            &dir,
        ));
    }
    if !metadata.is_dir() {
        return Err(path_error(
            io::ErrorKind::InvalidInput,
            "draft path is not a directory",
            &dir,
        ));
    }

    let canonical_dir = fs::canonicalize(&dir)
        .map_err(|error| contextual_error(error, "canonicalize draft directory", &dir))?;
    if canonical_dir.parent() != Some(canonical_root) {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft directory is not directly under the draft root",
            &dir,
        ));
    }
    let verified_metadata = fs::symlink_metadata(&canonical_dir).map_err(|error| {
        contextual_error(error, "reinspect canonical draft directory", &canonical_dir)
    })?;
    if verified_metadata.file_type().is_symlink() || !verified_metadata.is_dir() {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "canonical draft directory is not a real directory",
            &canonical_dir,
        ));
    }
    Ok(Some(canonical_dir))
}

pub(super) fn safe_project_entry(canonical_dir: &Path) -> io::Result<bool> {
    let project = canonical_dir.join(PROJECT_FILE);
    match fs::symlink_metadata(&project) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft project file must not be a symlink",
            &project,
        )),
        Ok(metadata) if !metadata.is_file() => Err(path_error(
            io::ErrorKind::InvalidInput,
            "draft project path is not a regular file",
            &project,
        )),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(contextual_error(
            error,
            "inspect draft project file",
            &project,
        )),
    }
}

pub(super) fn ensure_safe_meta_entry(canonical_dir: &Path) -> io::Result<()> {
    let meta = canonical_dir.join(META_FILE);
    match fs::symlink_metadata(&meta) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft metadata file must not be a symlink",
            &meta,
        )),
        Ok(metadata) if !metadata.is_file() => Err(path_error(
            io::ErrorKind::InvalidInput,
            "draft metadata path is not a regular file",
            &meta,
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(contextual_error(
            error,
            "inspect draft metadata file",
            &meta,
        )),
    }
}

pub(super) fn cleanup_created_slot(slot: &DraftSlot) -> io::Result<bool> {
    if slot.canonical_dir.parent() != Some(slot.canonical_root.as_path()) {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "refusing cleanup outside the canonical draft root",
            &slot.canonical_dir,
        ));
    }
    let metadata = match fs::symlink_metadata(&slot.canonical_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(contextual_error(
                error,
                "inspect failed import draft",
                &slot.canonical_dir,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "failed import draft is no longer a real directory",
            &slot.canonical_dir,
        ));
    }
    safe_project_entry(&slot.canonical_dir)?;
    tombstone_and_remove(&slot.canonical_root, &slot.canonical_dir)
}

pub(super) fn tombstone_and_remove(
    canonical_root: &Path,
    canonical_dir: &Path,
) -> io::Result<bool> {
    if canonical_dir.parent() != Some(canonical_root) {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "refusing to tombstone a directory outside the draft root",
            canonical_dir,
        ));
    }
    let metadata = match fs::symlink_metadata(canonical_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(contextual_error(
                error,
                "inspect draft before tombstoning",
                canonical_dir,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft changed before tombstoning",
            canonical_dir,
        ));
    }

    let tombstone = {
        let mut selected = None;
        for _ in 0..UNIQUE_PATH_ATTEMPTS {
            let candidate = canonical_root.join(format!(
                "{TOMBSTONE_PREFIX}{:x}-{}",
                std::process::id(),
                new_id()
            ));
            match fs::symlink_metadata(&candidate) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    selected = Some(candidate);
                    break;
                }
                Ok(_) => continue,
                Err(error) => {
                    return Err(contextual_error(
                        error,
                        "inspect prospective draft tombstone",
                        &candidate,
                    ));
                }
            }
        }
        selected.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a unique draft tombstone",
            )
        })?
    };

    match fs::rename(canonical_dir, &tombstone) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return match fs::symlink_metadata(canonical_dir) {
                Err(check_error) if check_error.kind() == io::ErrorKind::NotFound => Ok(false),
                _ => Err(contextual_error(
                    error,
                    "rename draft to tombstone",
                    canonical_dir,
                )),
            };
        }
        Err(error) => {
            return Err(contextual_error(
                error,
                "rename draft to tombstone",
                canonical_dir,
            ));
        }
    }

    if let Err(error) = cleanup_tombstone(canonical_root, &tombstone) {
        warn!(
            path = %bounded_path(&tombstone),
            error = %error,
            "draft deletion committed but tombstone cleanup failed"
        );
    } else if let Err(error) = sync_directory(canonical_root) {
        warn!(
            path = %bounded_path(canonical_root),
            error = %error,
            "draft deletion committed but root directory sync failed"
        );
    }
    Ok(true)
}

pub(super) fn cleanup_tombstone(canonical_root: &Path, tombstone: &Path) -> io::Result<()> {
    if tombstone.parent() != Some(canonical_root) {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "refusing to clean a tombstone outside the draft root",
            tombstone,
        ));
    }
    let metadata = fs::symlink_metadata(tombstone)
        .map_err(|error| contextual_error(error, "inspect draft tombstone", tombstone))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(path_error(
            io::ErrorKind::PermissionDenied,
            "draft tombstone is not a real directory",
            tombstone,
        ));
    }
    // Keep the exact generated path: `remove_dir_all` does not follow
    // symlinks, while canonicalizing again would reintroduce a race that
    // could redirect cleanup to a different directory.
    fs::remove_dir_all(tombstone)
        .map_err(|error| contextual_error(error, "recursively remove draft tombstone", tombstone))
}

pub(super) fn invalid_project_path(project: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "invalid draft project path '{}': {reason}",
            bounded_path(project)
        ),
    )
}

pub(super) fn path_error(kind: io::ErrorKind, action: &str, path: &Path) -> io::Error {
    io::Error::new(kind, format!("{action}: '{}'", bounded_path(path)))
}

pub(super) fn contextual_error(error: io::Error, action: &str, path: &Path) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{action} '{}': {error}", bounded_path(path)),
    )
}

pub(super) fn bounded_path(path: &Path) -> String {
    let lossy = path.as_os_str().to_string_lossy();
    let mut characters = lossy.chars();
    let mut bounded: String = characters.by_ref().take(MAX_ERROR_PATH_CHARS).collect();
    if characters.next().is_some() {
        bounded.push('…');
    }
    bounded
}

#[cfg(unix)]
pub(super) fn sync_directory(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
pub(super) fn sync_directory(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// A unique-per-process draft id: millis since the epoch plus a monotonic
/// counter, so rapid successive creates never share a directory name.
pub(super) fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{millis:x}-{seq:x}")
}
