//! Recent-projects MRU (lifecycle roadmap Phase 3).
//!
//! The last [`MAX_ENTRIES`] `.cutlass` paths, newest first, persisted as a
//! plain JSON array at `~/.cutlass/recent.json`. The worker notes every
//! successful save and open (the moments a path is proven real and current)
//! and republishes the list to `EditorStore.recent-projects`; reads prune
//! entries whose files no longer exist, so the File menu and the welcome
//! panel never offer a dead path.

use std::path::{Path, PathBuf};

use tracing::warn;

/// Most paths kept in the list.
pub const MAX_ENTRIES: usize = 10;

/// `~/.cutlass/recent.json` (HOME-relative; falls back to the working
/// directory when HOME is unset, mirroring the autosave sidecar dir).
pub fn default_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cutlass")
        .join("recent.json")
}

/// Read the MRU list: newest first, entries whose files are gone pruned,
/// capped at [`MAX_ENTRIES`]. A missing or unreadable file is an empty
/// list — recents are a convenience, never an error.
pub fn read(path: &Path) -> Vec<PathBuf> {
    let Ok(json) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(entries) = serde_json::from_str::<Vec<PathBuf>>(&json) else {
        warn!(path = %path.display(), "recent-projects file unparsable; treating as empty");
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|p| p.exists())
        .take(MAX_ENTRIES)
        .collect()
}

/// Move `project` to the front of the list at `path` (dedup, prune, cap)
/// and persist it. Returns the updated list so the caller can republish
/// without a second read. Write failures only log: losing a recents update
/// must never interrupt a save or open.
pub fn note(path: &Path, project: &Path) -> Vec<PathBuf> {
    let mut entries = read(path);
    entries.retain(|p| p != project);
    entries.insert(0, project.to_path_buf());
    entries.truncate(MAX_ENTRIES);

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(dir = %parent.display(), "recents update skipped: couldn't create dir: {e}");
        return entries;
    }
    let json = serde_json::to_string_pretty(&entries).expect("Vec<PathBuf> serializes");
    if let Err(e) = std::fs::write(path, json) {
        warn!(path = %path.display(), "recents update failed: {e}");
    }
    entries
}

/// The Slint rows for `entries`: file stem for display, full path for the
/// open callback. Shared by the launch load (main.rs) and the worker's
/// post-save/open republish.
pub fn to_rows(entries: &[PathBuf]) -> Vec<crate::RecentProject> {
    entries
        .iter()
        .map(|p| crate::RecentProject {
            name: p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
            path: p.to_string_lossy().into_owned().into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::write(path, b"{}").expect("write");
    }

    #[test]
    fn note_orders_newest_first_and_dedups() {
        let dir = tempfile::tempdir().expect("tempdir");
        let list = dir.path().join("recent.json");
        let a = dir.path().join("a.cutlass");
        let b = dir.path().join("b.cutlass");
        touch(&a);
        touch(&b);

        note(&list, &a);
        note(&list, &b);
        assert_eq!(read(&list), vec![b.clone(), a.clone()]);

        // Re-noting an existing entry moves it to the front, no duplicate.
        note(&list, &a);
        assert_eq!(read(&list), vec![a, b]);
    }

    #[test]
    fn read_prunes_missing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let list = dir.path().join("recent.json");
        let kept = dir.path().join("kept.cutlass");
        let gone = dir.path().join("gone.cutlass");
        touch(&kept);
        touch(&gone);
        note(&list, &kept);
        note(&list, &gone);

        std::fs::remove_file(&gone).expect("remove");
        assert_eq!(read(&list), vec![kept]);
    }

    #[test]
    fn list_caps_at_max_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let list = dir.path().join("recent.json");
        for i in 0..(MAX_ENTRIES + 3) {
            let p = dir.path().join(format!("p{i}.cutlass"));
            touch(&p);
            note(&list, &p);
        }
        let entries = read(&list);
        assert_eq!(entries.len(), MAX_ENTRIES);
        // Newest survives, oldest fell off.
        assert_eq!(
            entries[0],
            dir.path().join(format!("p{}.cutlass", MAX_ENTRIES + 2))
        );
        assert!(!entries.contains(&dir.path().join("p0.cutlass")));
    }

    #[test]
    fn missing_or_corrupt_file_reads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let list = dir.path().join("recent.json");
        assert_eq!(read(&list), Vec::<PathBuf>::new());

        std::fs::write(&list, b"not json").expect("write");
        assert_eq!(read(&list), Vec::<PathBuf>::new());
    }

    #[test]
    fn note_creates_parent_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let list = dir.path().join("nested").join("recent.json");
        let a = dir.path().join("a.cutlass");
        touch(&a);
        let entries = note(&list, &a);
        assert_eq!(entries, vec![a.clone()]);
        assert_eq!(read(&list), vec![a]);
    }
}
