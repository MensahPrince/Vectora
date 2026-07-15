//! The quota-managed download cache.
//!
//! Stock files, template bundles, and packs land here before import.
//! Unbounded data-dir growth is a real complaint generator, so the cache
//! enforces a byte quota with LRU eviction (by modification time — files
//! get re-touched on use). Files *imported into a project* are copied out
//! by the import path and become pool media; nothing in a project ever
//! references a cache path, so eviction is always safe.
//!
//! Settings surfaces a "clear download cache" action backed by
//! [`DownloadCache::clear`].

use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::error::CloudError;

/// Default quota: 2 GiB of downloaded stock/bundles before eviction.
pub const DEFAULT_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A directory with a byte budget.
pub struct DownloadCache {
    root: PathBuf,
    quota_bytes: AtomicU64,
    protected_paths: RwLock<HashSet<PathBuf>>,
}

/// Exact accounting for an explicit download-cache clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadCacheClear {
    /// Logical bytes removed from non-directory entries.
    pub removed_bytes: u64,
    /// Number of non-directory entries removed.
    pub removed_files: u64,
    /// Existing protected files retained in the cache.
    pub retained_protected_files: u64,
}

impl DownloadCache {
    pub fn new(root: PathBuf, quota_bytes: u64) -> Self {
        Self {
            root,
            quota_bytes: AtomicU64::new(quota_bytes),
            protected_paths: RwLock::new(HashSet::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the current byte quota.
    ///
    /// Quota enforcement snapshots this value once per invocation, so a
    /// concurrent update takes effect on a subsequent invocation.
    #[must_use]
    pub fn quota_bytes(&self) -> u64 {
        self.quota_bytes.load(Ordering::Relaxed)
    }

    /// Replaces the byte quota used by subsequent quota enforcement.
    ///
    /// An enforcement already in progress continues using the quota it
    /// snapshotted when it started.
    pub fn set_quota_bytes(&self, new: u64) {
        self.quota_bytes.store(new, Ordering::Relaxed);
    }

    /// Keep a cache-owned source file available while a project references it.
    ///
    /// Missing paths may be protected so a later re-download cannot be
    /// evicted before the project is migrated. Paths outside this cache, the
    /// cache root itself, and paths containing traversal components fail
    /// closed.
    pub fn protect_path(&self, path: &Path) -> Result<(), CloudError> {
        let path = contained_cache_path(&self.root, path)?;
        if let Ok(metadata) = std::fs::symlink_metadata(&path)
            && !metadata.file_type().is_file()
        {
            return Err(invalid_cache_path());
        }
        write_protected(&self.protected_paths).insert(path);
        Ok(())
    }

    /// Number of project-referenced cache paths currently protected.
    #[must_use]
    pub fn protected_path_count(&self) -> usize {
        read_protected(&self.protected_paths).len()
    }

    /// Where a download for `key` (e.g. `stock/pexels-857191-hd.mp4`)
    /// should land. Creates parent directories.
    pub fn path_for(&self, key: &str) -> Result<PathBuf, CloudError> {
        let key = validated_key(key)?;
        ensure_cache_root(&self.root)?;
        let parent = key.parent().unwrap_or_else(|| Path::new(""));
        ensure_safe_subdirectories(&self.root, parent)?;
        let path = self.root.join(key);
        reject_symlink_or_directory(&path)?;
        Ok(path)
    }

    /// Whether `key` is already downloaded (touches it for LRU).
    pub fn hit(&self, key: &str) -> Option<PathBuf> {
        let key = validated_key(key).ok()?;
        if !safe_existing_root(&self.root) {
            return None;
        }
        ensure_existing_safe_subdirectories(
            &self.root,
            key.parent().unwrap_or_else(|| Path::new("")),
        )
        .ok()?;
        let path = self.root.join(key);
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_file() {
            // Re-touch so recently-used files survive eviction.
            let _ = filetime_touch(&path);
            Some(path)
        } else {
            None
        }
    }

    /// Total bytes currently cached.
    pub fn size_bytes(&self) -> u64 {
        saturating_total_bytes(
            walk_files(&self.root)
                .into_iter()
                .filter_map(|p| std::fs::symlink_metadata(p).ok())
                .map(|m| m.len()),
        )
    }

    /// Evict least-recently-modified files until under quota. Called after
    /// each completed download; cheap when under budget.
    pub fn enforce_quota(&self) {
        let quota_bytes = self.quota_bytes();
        let protected = read_protected(&self.protected_paths).clone();
        let mut files: Vec<(PathBuf, u64, SystemTime)> = walk_files(&self.root)
            .into_iter()
            .filter(|path| !protected.contains(path))
            .filter_map(|p| {
                let meta = std::fs::symlink_metadata(&p).ok()?;
                let mtime = meta.modified().ok()?;
                Some((p, meta.len(), mtime))
            })
            .collect();
        let mut total = saturating_total_bytes(files.iter().map(|(_, len, _)| *len));
        if total <= quota_bytes {
            return;
        }
        // Oldest first.
        files.sort_by_key(|(_, _, mtime)| *mtime);
        for (path, len, _) in files {
            if total <= quota_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                tracing::debug!("cache evicted {}", path.display());
                total = total.saturating_sub(len);
            }
        }
    }

    /// Delete everything (the settings "clear download cache" action).
    /// The root itself stays so in-flight paths remain valid.
    pub fn clear(&self) -> Result<DownloadCacheClear, CloudError> {
        if safe_root_entries(&self.root)?.is_none() {
            return Ok(DownloadCacheClear {
                removed_bytes: 0,
                removed_files: 0,
                retained_protected_files: 0,
            });
        }
        let protected = read_protected(&self.protected_paths).clone();
        let (files, mut directories) = walk_tree(&self.root);
        let mut removed_bytes = 0_u64;
        let mut removed_files = 0_u64;
        let mut retained_protected_files = 0_u64;
        for path in files {
            if protected.contains(&path) {
                retained_protected_files = retained_protected_files.saturating_add(1);
                continue;
            }
            let bytes = std::fs::symlink_metadata(&path)?.len();
            std::fs::remove_file(path)?;
            removed_bytes = removed_bytes.saturating_add(bytes);
            removed_files = removed_files.saturating_add(1);
        }
        directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for path in directories {
            match std::fs::remove_dir(path) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(DownloadCacheClear {
            removed_bytes,
            removed_files,
            retained_protected_files,
        })
    }
}

const MAX_CACHE_KEY_BYTES: usize = 1_024;

fn validated_key(key: &str) -> Result<&Path, CloudError> {
    let path = Path::new(key);
    let valid = !key.is_empty()
        && key.len() <= MAX_CACHE_KEY_BYTES
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if valid {
        Ok(path)
    } else {
        Err(CloudError::Protocol(
            "invalid download cache key".to_owned(),
        ))
    }
}

fn contained_cache_path(root: &Path, path: &Path) -> Result<PathBuf, CloudError> {
    if !root.is_absolute() || !path.is_absolute() {
        return Err(invalid_cache_path());
    }
    let relative = path.strip_prefix(root).map_err(|_| invalid_cache_path())?;
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(invalid_cache_path());
    }
    Ok(root.join(relative))
}

fn read_protected(
    protected: &RwLock<HashSet<PathBuf>>,
) -> std::sync::RwLockReadGuard<'_, HashSet<PathBuf>> {
    protected
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_protected(
    protected: &RwLock<HashSet<PathBuf>>,
) -> std::sync::RwLockWriteGuard<'_, HashSet<PathBuf>> {
    protected
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ensure_cache_root(root: &Path) -> Result<(), CloudError> {
    if !root.is_absolute() {
        return Err(invalid_cache_root());
    }
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(invalid_cache_root()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(root)?;
            let metadata = std::fs::symlink_metadata(root)?;
            if metadata.file_type().is_dir() {
                Ok(())
            } else {
                Err(invalid_cache_root())
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn safe_existing_root(root: &Path) -> bool {
    root.is_absolute()
        && std::fs::symlink_metadata(root).is_ok_and(|metadata| metadata.file_type().is_dir())
}

fn ensure_safe_subdirectories(root: &Path, relative: &Path) -> Result<(), CloudError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(invalid_cache_path());
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(invalid_cache_path()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
                let metadata = std::fs::symlink_metadata(&current)?;
                if !metadata.file_type().is_dir() {
                    return Err(invalid_cache_path());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn ensure_existing_safe_subdirectories(root: &Path, relative: &Path) -> Result<(), ()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(());
        };
        current.push(name);
        let metadata = std::fs::symlink_metadata(&current).map_err(|_| ())?;
        if !metadata.file_type().is_dir() {
            return Err(());
        }
    }
    Ok(())
}

fn reject_symlink_or_directory(path: &Path) -> Result<(), CloudError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(invalid_cache_path()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn safe_root_entries(root: &Path) -> Result<Option<std::fs::ReadDir>, CloudError> {
    if !root.is_absolute() {
        return Err(invalid_cache_root());
    }
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            std::fs::read_dir(root).map(Some).map_err(Into::into)
        }
        Ok(_) => Err(invalid_cache_root()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn invalid_cache_root() -> CloudError {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "download cache root must be an absolute, real directory",
    )
    .into()
}

fn invalid_cache_path() -> CloudError {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "download cache path crosses an unsafe filesystem entry",
    )
    .into()
}

fn saturating_total_bytes(lengths: impl IntoIterator<Item = u64>) -> u64 {
    lengths
        .into_iter()
        .fold(0, |total, len| total.saturating_add(len))
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    walk_tree(root).0
}

fn walk_tree(root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    if !safe_existing_root(root) {
        return (Vec::new(), Vec::new());
    }
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(path.clone());
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    (files, directories)
}

/// Bump a file's mtime to now (LRU touch). `File::set_times` needs no
/// extra dependency.
fn filetime_touch(path: &Path) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new().append(true).open(path)?;
    let now = std::fs::FileTimes::new().set_modified(SystemTime::now());
    file.set_times(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn write(cache: &DownloadCache, key: &str, len: usize) -> PathBuf {
        let path = cache.path_for(key).unwrap();
        std::fs::write(&path, vec![0u8; len]).unwrap();
        path
    }

    fn backdate(path: &Path) {
        let earlier = SystemTime::now() - std::time::Duration::from_secs(120);
        let file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(earlier))
            .unwrap();
    }

    #[test]
    fn quota_evicts_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DownloadCache::new(dir.path().to_path_buf(), 250);
        let old = write(&cache, "stock/old.bin", 100);
        // Ensure distinct mtimes even on coarse filesystems.
        backdate(&old);
        let new = write(&cache, "stock/new.bin", 200);

        cache.enforce_quota();
        assert!(!old.exists(), "oldest file evicted");
        assert!(new.exists(), "newest file kept");
        assert_eq!(cache.size_bytes(), 200);
    }

    #[test]
    fn quota_updates_apply_to_subsequent_enforcement() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DownloadCache::new(dir.path().to_path_buf(), 300);
        let old = write(&cache, "stock/old.bin", 100);
        backdate(&old);
        let new = write(&cache, "stock/new.bin", 200);

        assert_eq!(cache.quota_bytes(), 300);
        cache.enforce_quota();
        assert!(old.exists());
        assert!(new.exists());

        cache.set_quota_bytes(200);
        assert_eq!(cache.quota_bytes(), 200);
        cache.enforce_quota();
        assert!(!old.exists(), "updated quota evicts the oldest file");
        assert!(new.exists(), "updated quota does not over-evict");
        assert_eq!(cache.size_bytes(), 200);
    }

    #[test]
    fn quota_is_safe_to_read_and_update_from_concurrent_threads() {
        const WORKERS: usize = 4;
        const ROUNDS: u64 = 64;

        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(DownloadCache::new(dir.path().to_path_buf(), 0));
        let phase = Arc::new(Barrier::new(WORKERS));
        let handles: Vec<_> = (0..WORKERS)
            .map(|worker| {
                let cache = Arc::clone(&cache);
                let phase = Arc::clone(&phase);
                thread::spawn(move || {
                    let mut observed_quotas = Vec::with_capacity(ROUNDS as usize);
                    for round in 0..ROUNDS {
                        phase.wait();
                        let first_quota = round * WORKERS as u64;
                        cache.set_quota_bytes(first_quota + worker as u64);
                        phase.wait();
                        observed_quotas.push(cache.quota_bytes());
                        phase.wait();
                    }
                    observed_quotas
                })
            })
            .collect();

        for handle in handles {
            for (round, observed) in handle.join().unwrap().into_iter().enumerate() {
                let first_quota = round as u64 * WORKERS as u64;
                let quotas_this_round = first_quota..first_quota + WORKERS as u64;
                assert!(
                    quotas_this_round.contains(&observed),
                    "observed quota {observed} outside {quotas_this_round:?}"
                );
            }
        }
    }

    #[test]
    fn total_size_saturates_instead_of_overflowing() {
        assert_eq!(saturating_total_bytes([u64::MAX - 1, 1, 1]), u64::MAX);
    }

    #[test]
    fn protected_project_media_survives_eviction_and_explicit_clear() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DownloadCache::new(dir.path().to_path_buf(), 1);
        let protected = write(&cache, "stock/project.mp4", 100);
        backdate(&protected);
        let disposable = write(&cache, "stock/thumbnail.img", 20);
        cache.protect_path(&protected).unwrap();

        assert_eq!(cache.protected_path_count(), 1);
        cache.enforce_quota();
        assert!(protected.exists());
        assert!(!disposable.exists());

        let another = write(&cache, "ai/unused.png", 12);
        let report = cache.clear().unwrap();
        assert_eq!(report.removed_bytes, 12);
        assert_eq!(report.removed_files, 1);
        assert_eq!(report.retained_protected_files, 1);
        assert!(protected.exists());
        assert!(!another.exists());
    }

    #[test]
    fn missing_project_media_can_be_protected_before_redownload() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DownloadCache::new(dir.path().to_path_buf(), 0);
        let future = dir.path().join("stock/future.mp4");
        cache.protect_path(&future).unwrap();
        assert_eq!(cache.protected_path_count(), 1);

        write(&cache, "stock/future.mp4", 8);
        cache.enforce_quota();
        assert!(future.exists());
        let report = cache.clear().unwrap();
        assert_eq!(report.retained_protected_files, 1);
        assert!(future.exists());
    }

    #[test]
    fn protected_paths_must_be_contained_cache_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = dir.path().join("cache");
        let cache = DownloadCache::new(cache_root.clone(), 100);
        std::fs::create_dir_all(&cache_root).unwrap();

        for path in [
            dir.path().join("outside.mp4"),
            cache_root.clone(),
            cache_root.join("../outside.mp4"),
            PathBuf::from("relative.mp4"),
        ] {
            assert!(cache.protect_path(&path).is_err(), "{path:?}");
        }

        let directory = cache_root.join("directory");
        std::fs::create_dir(&directory).unwrap();
        assert!(cache.protect_path(&directory).is_err());
        assert_eq!(cache.protected_path_count(), 0);
    }

    #[test]
    fn under_quota_evicts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DownloadCache::new(dir.path().to_path_buf(), 1000);
        let a = write(&cache, "a.bin", 100);
        cache.enforce_quota();
        assert!(a.exists());
    }

    #[test]
    fn hit_returns_existing_and_misses_absent() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DownloadCache::new(dir.path().to_path_buf(), 1000);
        assert!(cache.hit("nope.bin").is_none());
        write(&cache, "yes.bin", 10);
        assert!(cache.hit("yes.bin").is_some());
    }

    #[test]
    fn cache_keys_cannot_escape_or_replace_directories() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = dir.path().join("cache");
        let cache = DownloadCache::new(cache_root.clone(), 1_000);

        for key in [
            "",
            ".",
            "..",
            "../escaped.bin",
            "stock/../../escaped.bin",
            "/absolute.bin",
        ] {
            assert!(cache.path_for(key).is_err(), "{key:?} must be rejected");
            assert!(cache.hit(key).is_none(), "{key:?} cannot be a cache hit");
        }
        assert!(!dir.path().join("escaped.bin").exists());

        std::fs::create_dir_all(cache_root.join("occupied")).unwrap();
        assert!(cache.path_for("occupied").is_err());
    }

    #[test]
    fn relative_cache_roots_fail_closed() {
        let cache = DownloadCache::new(PathBuf::from("relative-cache"), 1_000);
        assert!(cache.path_for("asset.bin").is_err());
        assert!(cache.hit("asset.bin").is_none());
        assert_eq!(cache.size_bytes(), 0);
        assert!(cache.clear().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn nested_symlinks_are_never_followed_or_cleared_through() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cache_root = dir.path().join("cache");
        std::fs::create_dir(&cache_root).unwrap();
        let protected = outside.path().join("protected.bin");
        std::fs::write(&protected, b"keep").unwrap();
        symlink(outside.path(), cache_root.join("linked")).unwrap();

        let cache = DownloadCache::new(cache_root.clone(), 0);
        assert!(cache.path_for("linked/new.bin").is_err());
        assert!(cache.hit("linked/protected.bin").is_none());

        cache.enforce_quota();
        assert!(protected.exists(), "eviction must not traverse the symlink");

        if !cache_root.join("linked").exists() {
            symlink(outside.path(), cache_root.join("linked")).unwrap();
        }
        cache.clear().unwrap();
        assert!(protected.exists(), "clearing must not traverse the symlink");
        assert!(!cache_root.join("linked").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cache_root_fails_closed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let protected = outside.path().join("protected.bin");
        std::fs::write(&protected, b"keep").unwrap();
        let cache_root = dir.path().join("cache-link");
        symlink(outside.path(), &cache_root).unwrap();
        let cache = DownloadCache::new(cache_root, 0);

        assert!(cache.path_for("new.bin").is_err());
        assert!(cache.hit("protected.bin").is_none());
        assert_eq!(cache.size_bytes(), 0);
        cache.enforce_quota();
        assert!(cache.clear().is_err());
        assert!(protected.exists());
    }

    #[test]
    fn clear_empties_but_keeps_root() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DownloadCache::new(dir.path().to_path_buf(), 1000);
        write(&cache, "stock/a.bin", 10);
        write(&cache, "bundles/b.bin", 10);
        cache.clear().unwrap();
        assert_eq!(cache.size_bytes(), 0);
        assert!(cache.root().exists());
    }
}
