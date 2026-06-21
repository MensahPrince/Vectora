//! Autosave sidecars & crash recovery (lifecycle roadmap Phase 4).
//!
//! Dirty sessions snapshot to an `autosave/` dir in the per-user OS data dir
//! (see [`crate::paths`]) on a periodic sweep — never to the user's file.
//! Each slot is a plain `.cutlass` project plus a
//! `.meta.json` sidecar naming the file it stands in for (`None` for a
//! session that was never saved). On launch, [`newest_candidate`] finds
//! work worth offering back: an orphan from an unsaved session, or a slot
//! newer than its source file.
//!
//! Slot identity: saved projects hash their absolute path (stable across
//! runs — the slot survives a crash); unsaved sessions key on the process
//! id (stable within a run, orphaned by a crash — which is exactly what
//! recovery looks for). The worker owns writes; `main.rs` owns the launch
//! scan and the explicit discard on a "Don't Save" close.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// `autosave/` in the per-user OS data dir (see [`crate::paths`]):
/// `%APPDATA%\Cutlass` on Windows, `~/Library/Application Support/Cutlass` on
/// macOS, `~/.local/share/Cutlass` on Linux.
pub fn default_dir() -> PathBuf {
    crate::paths::data_dir().join("autosave")
}

/// Sidecar naming the file a slot stands in for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotMeta {
    /// The user's `.cutlass` path, or `None` for a never-saved session.
    pub source: Option<PathBuf>,
}

/// Unsaved work found on disk at launch.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryCandidate {
    /// The autosave snapshot to restore from.
    pub autosave: PathBuf,
    /// The file the snapshot stands in for (`None`: never-saved session).
    pub source: Option<PathBuf>,
    /// Snapshot write time (newest candidate wins).
    pub modified: SystemTime,
}

/// The autosave slot for a session: one stable path per project file
/// (path-hashed), one per process for never-saved sessions.
pub fn slot_for(dir: &Path, source: Option<&Path>) -> PathBuf {
    match source {
        Some(path) => dir.join(format!("{}.cutlass", fnv1a_hex(&path.to_string_lossy()))),
        None => dir.join(format!("unsaved-{}.cutlass", std::process::id())),
    }
}

fn meta_path(slot: &Path) -> PathBuf {
    let mut name = slot
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".meta.json");
    slot.with_file_name(name)
}

/// Record which file `slot` stands in for. The meta lands after the
/// snapshot itself, so a torn write degrades to "no candidate", never to a
/// restore pointing at the wrong file.
pub fn write_meta(slot: &Path, source: Option<&Path>) -> std::io::Result<()> {
    let meta = SlotMeta {
        source: source.map(Path::to_path_buf),
    };
    let json = serde_json::to_string(&meta).expect("SlotMeta serializes");
    std::fs::write(meta_path(slot), json)
}

/// Remove a slot and its meta. Missing files are fine (double discard,
/// clean-session sweep on a slot that never existed).
pub fn discard(slot: &Path) {
    let _ = std::fs::remove_file(slot);
    let _ = std::fs::remove_file(meta_path(slot));
}

/// Scan `dir` for the newest snapshot worth offering back: an orphan from
/// a never-saved session, a slot whose source file is gone, or a slot
/// written after its source was last saved. Slots without readable meta
/// are skipped (torn write — the snapshot may predate the meta).
pub fn newest_candidate(dir: &Path) -> Option<RecoveryCandidate> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<RecoveryCandidate> = None;
    for entry in entries.filter_map(|e| e.ok()) {
        let slot = entry.path();
        if slot.extension().is_none_or(|ext| ext != "cutlass") {
            continue;
        }
        let Ok(meta_json) = std::fs::read_to_string(meta_path(&slot)) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<SlotMeta>(&meta_json) else {
            continue;
        };
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let worth_offering = match &meta.source {
            // Never-saved session: any snapshot is unsaved work.
            None => true,
            Some(source) => match std::fs::metadata(source).and_then(|m| m.modified()) {
                // Saved after the snapshot ⇒ the snapshot is stale.
                Ok(source_modified) => modified > source_modified,
                // Source gone: the snapshot is all that's left.
                Err(_) => true,
            },
        };
        if !worth_offering {
            continue;
        }
        if best.as_ref().is_none_or(|b| modified > b.modified) {
            best = Some(RecoveryCandidate {
                autosave: slot,
                source: meta.source,
                modified,
            });
        }
    }
    best
}

/// FNV-1a over the path string — a stable, dependency-free slot key. Not
/// cryptographic; a collision merely shares a slot between two projects,
/// and the meta still names the right source.
fn fnv1a_hex(s: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::write(path, b"{}").expect("write");
    }

    #[test]
    fn slot_naming_is_stable_per_source_and_distinct() {
        let dir = Path::new("/tmp/anywhere");
        let a1 = slot_for(dir, Some(Path::new("/p/a.cutlass")));
        let a2 = slot_for(dir, Some(Path::new("/p/a.cutlass")));
        let b = slot_for(dir, Some(Path::new("/p/b.cutlass")));
        assert_eq!(a1, a2, "same source, same slot across calls");
        assert_ne!(a1, b, "different sources get different slots");
        assert_ne!(a1, slot_for(dir, None), "unsaved slot is its own");
    }

    #[test]
    fn meta_roundtrip_and_discard() {
        let dir = tempfile::tempdir().expect("tempdir");
        let slot = slot_for(dir.path(), Some(Path::new("/p/a.cutlass")));
        touch(&slot);
        write_meta(&slot, Some(Path::new("/p/a.cutlass"))).expect("meta");
        assert!(meta_path(&slot).exists());
        discard(&slot);
        assert!(!slot.exists());
        assert!(!meta_path(&slot).exists());
        discard(&slot); // double discard is fine
    }

    #[test]
    fn orphan_unsaved_slot_is_a_candidate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let slot = dir.path().join("unsaved-12345.cutlass");
        touch(&slot);
        write_meta(&slot, None).expect("meta");
        let candidate = newest_candidate(dir.path()).expect("candidate");
        assert_eq!(candidate.autosave, slot);
        assert_eq!(candidate.source, None);
    }

    #[test]
    fn stale_slot_older_than_source_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("project.cutlass");
        let slot = slot_for(dir.path(), Some(&source));
        touch(&slot);
        write_meta(&slot, Some(&source)).expect("meta");
        // The source is written *after* the snapshot ⇒ snapshot is stale.
        std::thread::sleep(std::time::Duration::from_millis(20));
        touch(&source);
        assert_eq!(newest_candidate(dir.path()), None);
    }

    #[test]
    fn slot_newer_than_source_or_with_missing_source_is_offered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("project.cutlass");
        touch(&source);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let slot = slot_for(dir.path(), Some(&source));
        touch(&slot);
        write_meta(&slot, Some(&source)).expect("meta");
        let candidate = newest_candidate(dir.path()).expect("newer than source");
        assert_eq!(candidate.source.as_deref(), Some(source.as_path()));

        // Source vanishes ⇒ still (more than ever) a candidate.
        std::fs::remove_file(&source).expect("remove");
        assert!(newest_candidate(dir.path()).is_some());
    }

    #[test]
    fn newest_of_several_candidates_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join("unsaved-1.cutlass");
        touch(&old);
        write_meta(&old, None).expect("meta");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let new = dir.path().join("unsaved-2.cutlass");
        touch(&new);
        write_meta(&new, None).expect("meta");
        let candidate = newest_candidate(dir.path()).expect("candidate");
        assert_eq!(candidate.autosave, new);
    }

    #[test]
    fn slot_without_meta_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        touch(&dir.path().join("unsaved-9.cutlass"));
        assert_eq!(newest_candidate(dir.path()), None);
    }
}
