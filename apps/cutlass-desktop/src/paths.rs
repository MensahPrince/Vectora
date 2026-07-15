//! Per-user, platform-correct locations for Cutlass's writable data.
//!
//! On Windows the app installs into `C:\Program Files\Cutlass` (read-only for
//! normal users) and its shortcut launches with that as the working directory,
//! so any *relative* path or any `$HOME`-derived path (`HOME` is unset on
//! Windows) lands in a folder the app can't write to — the historic "Cutlass
//! crashes on launch unless you run as administrator" bug.
//!
//! This helper resolves to the OS-blessed per-user data directory instead,
//! which is always writable without elevation:
//!
//! | role  | Windows    | macOS                           | Linux (XDG)      |
//! |-------|------------|---------------------------------|------------------|
//! | data  | `%APPDATA%`| `~/Library/Application Support` | `~/.local/share` |
//!
//! Callers create the directory lazily (the draft store `create_dir_all`s its
//! target), so this function only computes a path.

use std::path::PathBuf;

use cutlass_storage::{CacheId, StorageError, StorageLayout};

/// Application folder nested under the OS data root.
const APP_DIR: &str = "Cutlass";

/// Writable **data** root for things the user would miss if they vanished:
/// the app-owned project drafts (`<os-data>/Cutlass`, see [`crate::drafts`]).
pub fn data_dir() -> PathBuf {
    app_root(dirs::data_dir())
}

/// Resolve cache storage from typed settings without touching the filesystem.
///
/// [`data_dir`] remains the authoritative user-data root for projects and
/// drafts. It is only the default cache root when `[storage].root` is absent.
#[allow(dead_code)] // Consumed by the following isolated Phase 2b slices.
pub fn storage_layout(
    settings: &cutlass_settings::StorageSettings,
) -> Result<StorageLayout, StorageError> {
    let root = settings.root.clone().unwrap_or_else(data_dir);
    let mut layout = StorageLayout::new(root)?;

    for (id, override_path) in [
        (CacheId::Proxies, settings.paths.proxies.as_deref()),
        (CacheId::Analysis, settings.paths.analysis.as_deref()),
        (CacheId::Download, settings.paths.download.as_deref()),
        (CacheId::Catalog, settings.paths.catalog.as_deref()),
        (CacheId::Luts, settings.paths.luts.as_deref()),
        (CacheId::Lottie, settings.paths.lottie.as_deref()),
        (CacheId::Templates, settings.paths.templates.as_deref()),
    ] {
        if let Some(path) = override_path {
            layout.set_override(id, path)?;
        }
    }

    Ok(layout)
}

/// Load the user config and resolve its cache layout.
///
/// Configuration failures are deliberately returned as a fixed, bounded
/// message: malformed config must not become defaults, while parse details
/// must not echo unrelated fields or secrets from the file.
#[allow(dead_code)] // Consumed by the following isolated Phase 2b slices.
pub fn current_storage_layout() -> Result<StorageLayout, String> {
    let settings = cutlass_settings::load(&cutlass_settings::default_config_path())
        .map_err(|_| String::from("could not load storage settings"))?;
    storage_layout(&settings.storage).map_err(|error| format!("invalid storage settings: {error}"))
}

/// Resolve one disk cache path, rejecting memory-only cache identifiers.
#[allow(dead_code)] // Consumed by the following isolated Phase 2b slices.
pub fn cache_path(layout: &StorageLayout, id: CacheId) -> Result<PathBuf, StorageError> {
    layout.resolve(id).ok_or(StorageError::CacheIsNotDisk(id))
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
    use cutlass_settings::{StoragePathOverrides, StorageSettings};

    const DISK_CACHE_DIRS: [(CacheId, &str); 7] = [
        (CacheId::Proxies, "proxies"),
        (CacheId::Analysis, "analysis"),
        (CacheId::Download, "download-cache"),
        (CacheId::Catalog, "catalog-cache"),
        (CacheId::Luts, "luts"),
        (CacheId::Lottie, "lottie"),
        (CacheId::Templates, "templates"),
    ];

    const MEMORY_CACHE_IDS: [CacheId; 4] = [
        CacheId::PreviewFrames,
        CacheId::LibraryThumbnails,
        CacheId::TimelineFilmstrips,
        CacheId::TimelineWaveforms,
    ];

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("cutlass-path-resolution-tests")
            .join(name)
    }

    #[test]
    fn data_dir_is_absolute_and_namespaced() {
        let data = data_dir();
        assert!(data.is_absolute(), "data dir must be absolute: {data:?}");
        assert!(data.ends_with("Cutlass"));
    }

    #[test]
    fn default_layout_keeps_existing_cache_directories() {
        let data = data_dir();
        let layout = storage_layout(&StorageSettings::default()).unwrap();

        assert_eq!(layout.root(), data.as_path());
        for (id, relative) in DISK_CACHE_DIRS {
            assert_eq!(cache_path(&layout, id).unwrap(), data.join(relative));
        }
    }

    #[test]
    fn configured_root_changes_only_cache_roots() {
        let unchanged_data = data_dir();
        let configured_root = test_path("configured-root");
        let settings = StorageSettings {
            root: Some(configured_root.clone()),
            ..StorageSettings::default()
        };

        let layout = storage_layout(&settings).unwrap();

        assert_eq!(layout.root(), configured_root.as_path());
        for (id, relative) in DISK_CACHE_DIRS {
            assert_eq!(
                cache_path(&layout, id).unwrap(),
                configured_root.join(relative)
            );
        }
        assert_eq!(data_dir(), unchanged_data);
        assert_eq!(
            data_dir().join("projects"),
            unchanged_data.join("projects"),
            "storage settings must not relocate project data"
        );
    }

    #[test]
    fn each_configured_override_wins() {
        let proxy_override = test_path("overrides/proxy");
        let analysis_override = test_path("overrides/analysis");
        let download_override = test_path("overrides/download");
        let catalog_override = test_path("overrides/catalog");
        let luts_override = test_path("overrides/luts");
        let lottie_override = test_path("overrides/lottie");
        let templates_override = test_path("overrides/templates");
        let settings = StorageSettings {
            root: Some(test_path("override-default-root")),
            paths: StoragePathOverrides {
                proxies: Some(proxy_override.clone()),
                analysis: Some(analysis_override.clone()),
                download: Some(download_override.clone()),
                catalog: Some(catalog_override.clone()),
                luts: Some(luts_override.clone()),
                lottie: Some(lottie_override.clone()),
                templates: Some(templates_override.clone()),
            },
            ..StorageSettings::default()
        };
        let expected = [
            (CacheId::Proxies, proxy_override),
            (CacheId::Analysis, analysis_override),
            (CacheId::Download, download_override),
            (CacheId::Catalog, catalog_override),
            (CacheId::Luts, luts_override),
            (CacheId::Lottie, lottie_override),
            (CacheId::Templates, templates_override),
        ];

        let layout = storage_layout(&settings).unwrap();

        for (id, path) in expected {
            assert_eq!(layout.override_for(id), Some(path.as_path()));
            assert_eq!(cache_path(&layout, id).unwrap(), path);
        }
    }

    #[test]
    fn memory_cache_ids_have_no_paths() {
        let layout = storage_layout(&StorageSettings::default()).unwrap();

        for id in MEMORY_CACHE_IDS {
            assert!(matches!(
                cache_path(&layout, id),
                Err(StorageError::CacheIsNotDisk(rejected)) if rejected == id
            ));
        }
    }

    #[test]
    fn filesystem_root_error_propagates_from_storage_layout() {
        let root = data_dir()
            .ancestors()
            .last()
            .expect("data_dir is absolute and has a filesystem root")
            .to_path_buf();
        let settings = StorageSettings {
            root: Some(root),
            ..StorageSettings::default()
        };

        assert!(matches!(
            storage_layout(&settings),
            Err(StorageError::DangerousPath(_))
        ));
    }

    #[test]
    fn overlapping_override_error_propagates_from_storage_layout() {
        let root = test_path("overlap-default-root");
        let settings = StorageSettings {
            root: Some(root.clone()),
            paths: StoragePathOverrides {
                proxies: Some(root.join("download-cache").join("nested")),
                ..StoragePathOverrides::default()
            },
            ..StorageSettings::default()
        };

        assert!(matches!(
            storage_layout(&settings),
            Err(StorageError::CachePathsOverlap {
                cache: CacheId::Proxies,
                other: CacheId::Download,
            })
        ));
    }
}
