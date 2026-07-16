#![allow(unused_imports)]

use std::cell::Cell;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use cutlass_engine::EngineConfig;
use slint::ComponentHandle;
use slint::Global;
use slint::Model;
use slint::ModelRc;
use slint::SharedString;
use slint::VecModel;

pub(crate) const MIB_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_CACHE_UI_ERROR_CHARS: usize = 160;
pub(crate) const CACHE_GENERATION_EXHAUSTED: &str =
    "Cache operations are unavailable until Cutlass restarts.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DownloadQuota {
    pub(crate) mib: u64,
    pub(crate) bytes: u64,
}

pub(crate) fn parse_download_quota_mib(input: &str) -> Result<DownloadQuota, String> {
    let invalid = || {
        format!(
            "Download quota must be a whole number between {} and {} MiB.",
            cutlass_settings::MIN_DOWNLOAD_QUOTA_MIB,
            cutlass_settings::MAX_DOWNLOAD_QUOTA_MIB
        )
    };
    let mib = input.trim().parse::<u64>().map_err(|_| invalid())?;
    if !cutlass_settings::StorageSettings::is_valid_download_quota_mib(mib) {
        return Err(invalid());
    }
    let bytes = mib.checked_mul(MIB_BYTES).ok_or_else(invalid)?;
    Ok(DownloadQuota { mib, bytes })
}

/// Reserve a generation for one accepted cache UI operation. `u64::MAX` is a
/// terminal sentinel: reserving it invalidates any older result and reports a
/// bounded error rather than wrapping back to zero.
pub(crate) fn next_cache_generation(generation: &AtomicU64) -> Result<u64, &'static str> {
    let mut current = generation.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(1) else {
            return Err(CACHE_GENERATION_EXHAUSTED);
        };
        if next == u64::MAX {
            match generation.compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Err(CACHE_GENERATION_EXHAUSTED),
                Err(actual) => {
                    current = actual;
                    continue;
                }
            }
        }
        match generation.compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(next),
            Err(actual) => current = actual,
        }
    }
}

pub(crate) fn format_bytes_iec(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let mut number = format!("{value:.1}");
    if number.ends_with(".0") {
        number.truncate(number.len() - 2);
    }
    format!("{number} {}", UNITS[unit])
}

pub(crate) fn pluralized_count(count: u64, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

pub(crate) fn resolve_chat_id(
    labels: &[SharedString],
    ids: &[SharedString],
    selected_label: &str,
) -> Option<String> {
    labels
        .iter()
        .position(|label| label.as_str() == selected_label)
        .and_then(|index| ids.get(index))
        .map(ToString::to_string)
}

pub(crate) fn cache_item_count_label(
    kind: cutlass_storage::CacheKind,
    entries: u64,
    files: u64,
) -> String {
    match kind {
        cutlass_storage::CacheKind::Memory => pluralized_count(entries, "entry", "entries"),
        cutlass_storage::CacheKind::Disk => pluralized_count(files, "file", "files"),
    }
}

pub(crate) fn cache_relocation_supported(id: cutlass_storage::CacheId) -> bool {
    matches!(
        id,
        cutlass_storage::CacheId::Proxies
            | cutlass_storage::CacheId::Analysis
            | cutlass_storage::CacheId::AiModels
            | cutlass_storage::CacheId::Download
            | cutlass_storage::CacheId::Catalog
            | cutlass_storage::CacheId::Luts
            | cutlass_storage::CacheId::Lottie
            | cutlass_storage::CacheId::Templates
    ) && id.descriptor().kind == cutlass_storage::CacheKind::Disk
        && id.descriptor().default_relative.is_some()
}

pub(crate) fn cache_relocation_destination(
    selected_parent: &std::path::Path,
    id: cutlass_storage::CacheId,
) -> Result<PathBuf, &'static str> {
    if !cache_relocation_supported(id) {
        return Err("cache target is not relocatable");
    }
    let relative = id
        .descriptor()
        .default_relative
        .ok_or("disk cache has no default relative path")?;
    Ok(selected_parent.join(relative))
}

pub(crate) fn cache_rows_from_snapshots(
    mut snapshots: Vec<crate::cache_registry::CacheSnapshot>,
) -> Result<Vec<crate::CacheUsageRow>, String> {
    snapshots.sort_by_key(|snapshot| snapshot.id);
    snapshots
        .into_iter()
        .map(|snapshot| {
            let path = match (snapshot.kind, snapshot.path.as_deref()) {
                (cutlass_storage::CacheKind::Memory, None) => String::new(),
                (cutlass_storage::CacheKind::Memory, Some(_)) => {
                    return Err("memory cache unexpectedly has a disk path".into());
                }
                (cutlass_storage::CacheKind::Disk, Some(path)) => {
                    path.to_string_lossy().into_owned()
                }
                (cutlass_storage::CacheKind::Disk, None) => {
                    return Err("disk cache has no storage path".into());
                }
            };
            Ok(crate::CacheUsageRow {
                id: snapshot.id.as_str().into(),
                label: snapshot.label.into(),
                kind: match snapshot.kind {
                    cutlass_storage::CacheKind::Memory => "memory".into(),
                    cutlass_storage::CacheKind::Disk => "disk".into(),
                },
                size_label: format_bytes_iec(snapshot.bytes).into(),
                item_count_label: cache_item_count_label(
                    snapshot.kind,
                    snapshot.entries,
                    snapshot.files,
                )
                .into(),
                path: path.into(),
                clearable: crate::cache_registry::cache_can_be_cleared(snapshot.id),
                relocatable: snapshot.kind == cutlass_storage::CacheKind::Disk
                    && cache_relocation_supported(snapshot.id),
            })
        })
        .collect()
}

pub(crate) fn cache_clear_success(report: &crate::cache_registry::CacheClearReport) -> String {
    let descriptor = report.id.descriptor();
    format!(
        "Cleared {}: removed {} and {}.",
        descriptor.label,
        format_bytes_iec(report.removed_bytes),
        cache_item_count_label(
            descriptor.kind,
            report.removed_entries,
            report.removed_files
        )
    )
}

pub(crate) fn cache_relocation_success(
    report: &crate::cache_registry::CacheRelocationReport,
) -> String {
    let mut status = format!(
        "Moved {} to {}: {} in {}.",
        report.id.descriptor().label,
        report.new_path.display(),
        format_bytes_iec(report.bytes),
        pluralized_count(report.files, "file", "files"),
    );
    if let Some(warning) = report.cleanup_warning.as_deref() {
        status.push_str(" Cleanup warning: ");
        status.push_str(&bounded_cache_ui_error(warning));
    }
    status
}

pub(crate) fn bounded_cache_ui_error(error: &str) -> String {
    let mut bounded: String = error.chars().take(MAX_CACHE_UI_ERROR_CHARS).collect();
    if error.chars().count() > MAX_CACHE_UI_ERROR_CHARS {
        bounded.push('…');
    }
    bounded
}

pub(crate) fn spawn_short_lived_worker(
    name: &'static str,
    task: impl FnOnce() + Send + 'static,
) -> Result<(), String> {
    let worker = std::thread::Builder::new()
        .name(name.into())
        .spawn(task)
        .map_err(|error| format!("could not start {name}: {error}"))?;
    drop(worker);
    Ok(())
}

pub(crate) async fn pick_cache_relocation_parent(
    label: &str,
    starting_directory: Option<PathBuf>,
) -> Option<PathBuf> {
    let title = format!("Choose a parent folder for {label}");
    let mut dialog = rfd::AsyncFileDialog::new().set_title(&title);
    if let Some(directory) = starting_directory.filter(|directory| directory.is_dir()) {
        dialog = dialog.set_directory(directory);
    }
    dialog
        .pick_folder()
        .await
        .map(|folder| folder.path().to_path_buf())
}

/// Native save dialog for the export destination, seeded with the current
/// path's folder and file name. `None` when the user cancels.
pub(crate) async fn pick_export_path(current: std::path::PathBuf) -> Option<std::path::PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new().add_filter("MP4 video", &["mp4"]);
    if let Some(dir) = current.parent().filter(|d| d.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog = dialog.set_file_name(
        current
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.mp4".into()),
    );
    dialog
        .save_file()
        .await
        .map(|file| file.path().to_path_buf())
}
