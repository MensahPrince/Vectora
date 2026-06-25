//! Per-user, platform-correct locations for Cutlass's writable data.
//!
//! On Windows the app installs into `C:\Program Files\Cutlass` (read-only for
//! normal users) and its shortcut launches with that as the working directory,
//! so any *relative* path (the engine's old `.cutlass/cache` default) or any
//! `$HOME`-derived path (`HOME` is unset on Windows) lands in a folder the app
//! can't write to. The first such write — the frame cache, created during
//! engine startup — then fails and the process exits instantly: the historic
//! "Cutlass crashes on launch unless you run as administrator" bug.
//!
//! These helpers resolve to the OS-blessed per-user directories instead, which
//! are always writable without elevation:
//!
//! | role  | Windows           | macOS                          | Linux (XDG)            |
//! |-------|-------------------|--------------------------------|------------------------|
//! | cache | `%LOCALAPPDATA%`  | `~/Library/Caches`             | `~/.cache`             |
//! | data  | `%APPDATA%`       | `~/Library/Application Support` | `~/.local/share`      |
//!
//! Callers create the directories lazily (`FrameCache::new` and the draft
//! store both `create_dir_all` their target), so these functions only compute
//! paths.

use std::path::PathBuf;

/// Application folder nested under the OS cache/data roots.
const APP_DIR: &str = "Cutlass";

/// Writable **cache** root for regenerable frame blobs and index sidecars
/// (`<os-cache>/Cutlass/cache`). Safe to delete; rebuilt on demand.
pub fn cache_dir() -> PathBuf {
    app_root(dirs::cache_dir()).join("cache")
}

/// Writable **data** root for things the user would miss if they vanished:
/// the app-owned project drafts (`<os-data>/Cutlass`, see [`crate::drafts`]).
pub fn data_dir() -> PathBuf {
    app_root(dirs::data_dir())
}

/// `<base>/Cutlass`, where `base` is the OS dir when known, else the user's
/// home, else the temp dir. Never the working directory — on Windows that is
/// the read-only install folder, the very thing this module exists to avoid.
fn app_root(base: Option<PathBuf>) -> PathBuf {
    base.or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_and_data_are_absolute_and_namespaced() {
        let cache = cache_dir();
        let data = data_dir();
        assert!(cache.is_absolute(), "cache dir must be absolute: {cache:?}");
        assert!(data.is_absolute(), "data dir must be absolute: {data:?}");
        assert!(cache.ends_with("Cutlass/cache"));
        assert!(data.ends_with("Cutlass"));
    }
}
