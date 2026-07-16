use super::*;

#[test]
fn chat_label_resolution_uses_parallel_ids_and_rejects_mismatches() {
    let labels = vec![
        SharedString::from("Trim the clip"),
        SharedString::from("Trim the clip · 2"),
    ];
    let ids = vec![SharedString::from("chat-20"), SharedString::from("chat-10")];

    assert_eq!(
        resolve_chat_id(&labels, &ids, "Trim the clip · 2"),
        Some("chat-10".to_string())
    );
    assert_eq!(
        resolve_chat_id(&labels, &ids[..1], "Trim the clip · 2"),
        None
    );
    assert_eq!(resolve_chat_id(&labels, &ids, "Missing"), None);
}

fn snapshot(
    id: cutlass_storage::CacheId,
    path: Option<PathBuf>,
    bytes: u64,
    entries: u64,
    files: u64,
) -> cache_registry::CacheSnapshot {
    let descriptor = id.descriptor();
    cache_registry::CacheSnapshot {
        id,
        label: descriptor.label,
        kind: descriptor.kind,
        tier: descriptor.tier,
        path,
        bytes,
        entries,
        files,
    }
}

#[test]
fn cache_byte_and_count_labels_use_iec_and_correct_plurals() {
    assert_eq!(format_bytes_iec(0), "0 B");
    assert_eq!(format_bytes_iec(1_023), "1023 B");
    assert_eq!(format_bytes_iec(1_024), "1 KiB");
    assert_eq!(format_bytes_iec(1_536), "1.5 KiB");
    assert_eq!(format_bytes_iec(1024 * 1024), "1 MiB");

    assert_eq!(
        cache_item_count_label(cutlass_storage::CacheKind::Memory, 1, 0),
        "1 entry"
    );
    assert_eq!(
        cache_item_count_label(cutlass_storage::CacheKind::Memory, 2, 0),
        "2 entries"
    );
    assert_eq!(
        cache_item_count_label(cutlass_storage::CacheKind::Disk, 0, 1),
        "1 file"
    );
    assert_eq!(
        cache_item_count_label(cutlass_storage::CacheKind::Disk, 0, 3),
        "3 files"
    );
}

#[test]
fn cache_rows_are_registry_ordered_with_exact_path_rules() {
    let ai_models_path = PathBuf::from("/tmp/cutlass-ai-models");
    let download_path = PathBuf::from("/tmp/cutlass-downloads");
    let rows = cache_rows_from_snapshots(vec![
        snapshot(
            cutlass_storage::CacheId::Download,
            Some(download_path.clone()),
            1_536,
            0,
            1,
        ),
        snapshot(
            cutlass_storage::CacheId::AiModels,
            Some(ai_models_path.clone()),
            13,
            0,
            1,
        ),
        snapshot(cutlass_storage::CacheId::PreviewFrames, None, 1_024, 2, 0),
    ])
    .unwrap();

    assert_eq!(rows[0].id.as_str(), "preview_frames");
    assert_eq!(rows[0].path.as_str(), "");
    assert_eq!(rows[0].size_label.as_str(), "1 KiB");
    assert_eq!(rows[0].item_count_label.as_str(), "2 entries");
    assert!(rows[0].clearable);
    assert!(!rows[0].relocatable);

    assert_eq!(rows[1].id.as_str(), "ai_models");
    assert_eq!(
        rows[1].path.as_str(),
        ai_models_path.to_str().expect("test path is Unicode")
    );
    assert_eq!(rows[1].label.as_str(), "AI models");
    assert_eq!(rows[1].size_label.as_str(), "13 B");
    assert_eq!(rows[1].item_count_label.as_str(), "1 file");
    assert!(rows[1].clearable);
    assert!(rows[1].relocatable);

    assert_eq!(rows[2].id.as_str(), "download");
    assert_eq!(
        rows[2].path.as_str(),
        download_path.to_str().expect("test path is Unicode")
    );
    assert_eq!(rows[2].size_label.as_str(), "1.5 KiB");
    assert_eq!(rows[2].item_count_label.as_str(), "1 file");
    assert!(rows[2].clearable);
    assert!(rows[2].relocatable);

    assert!(
        cache_rows_from_snapshots(vec![snapshot(
            cutlass_storage::CacheId::PreviewFrames,
            Some(PathBuf::from("/tmp/not-memory")),
            0,
            0,
            0,
        )])
        .unwrap_err()
        .contains("memory")
    );
    assert!(
        cache_rows_from_snapshots(vec![snapshot(
            cutlass_storage::CacheId::Download,
            None,
            0,
            0,
            0,
        )])
        .unwrap_err()
        .contains("no storage path")
    );
}

#[test]
fn cache_relocation_support_is_exactly_the_eight_disk_caches() {
    let supported = cutlass_storage::CacheId::ALL
        .into_iter()
        .filter(|id| cache_relocation_supported(*id))
        .collect::<Vec<_>>();
    assert_eq!(
        supported,
        vec![
            cutlass_storage::CacheId::Proxies,
            cutlass_storage::CacheId::Analysis,
            cutlass_storage::CacheId::AiModels,
            cutlass_storage::CacheId::Download,
            cutlass_storage::CacheId::Catalog,
            cutlass_storage::CacheId::Luts,
            cutlass_storage::CacheId::Lottie,
            cutlass_storage::CacheId::Templates,
        ]
    );
    assert!(supported.iter().all(|id| {
        let descriptor = id.descriptor();
        descriptor.kind == cutlass_storage::CacheKind::Disk && descriptor.default_relative.is_some()
    }));
}

#[test]
fn cache_relocation_destination_uses_the_selected_parent_and_default_leaf() {
    let parent = PathBuf::from("/chosen-parent");
    for id in [
        cutlass_storage::CacheId::Proxies,
        cutlass_storage::CacheId::Analysis,
        cutlass_storage::CacheId::AiModels,
        cutlass_storage::CacheId::Download,
        cutlass_storage::CacheId::Catalog,
        cutlass_storage::CacheId::Luts,
        cutlass_storage::CacheId::Lottie,
        cutlass_storage::CacheId::Templates,
    ] {
        let relative = id
            .descriptor()
            .default_relative
            .expect("supported disk cache has a default leaf");
        assert_eq!(
            cache_relocation_destination(&parent, id),
            Ok(parent.join(relative))
        );
    }
    assert_eq!(
        cache_relocation_destination(&parent, cutlass_storage::CacheId::PreviewFrames),
        Err("cache target is not relocatable")
    );
}

#[test]
fn cache_relocation_success_includes_accounting_destination_and_cleanup_warning() {
    let report = cache_registry::CacheRelocationReport {
        id: cutlass_storage::CacheId::Proxies,
        old_path: PathBuf::from("/old/proxies"),
        new_path: PathBuf::from("/new-parent/proxies"),
        bytes: 1_536,
        files: 2,
        used_copy_fallback: true,
        cleanup_warning: Some("The old cache directory could not be removed.".into()),
        generation: 7,
        current: None,
    };

    assert_eq!(
        cache_relocation_success(&report),
        "Moved Proxies to /new-parent/proxies: 1.5 KiB in 2 files. Cleanup warning: The old cache directory could not be removed."
    );
}

#[test]
fn cache_rows_mark_exactly_supported_disk_caches_relocatable() {
    let root = PathBuf::from("/cache-root");
    let snapshots = cutlass_storage::CacheId::ALL
        .into_iter()
        .map(|id| {
            let path = id
                .descriptor()
                .default_relative
                .map(|relative| root.join(relative));
            snapshot(id, path, 0, 0, 0)
        })
        .collect();
    let rows = cache_rows_from_snapshots(snapshots).unwrap();
    assert_eq!(rows.len(), 12);
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        cutlass_storage::cache_descriptors()
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.id.as_str() == "ai_models")
            .count(),
        1
    );
    let relocatable = rows
        .iter()
        .filter(|row| row.relocatable)
        .map(|row| row.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        relocatable,
        vec![
            "proxies",
            "analysis",
            "ai_models",
            "download",
            "catalog",
            "luts",
            "lottie",
            "templates"
        ]
    );
    assert!(
        rows.iter()
            .filter(|row| row.kind.as_str() == "memory")
            .all(|row| !row.relocatable)
    );
}

#[test]
fn quota_parser_accepts_only_supported_integer_mib_values() {
    assert_eq!(
        parse_download_quota_mib(" 2048 ").unwrap(),
        DownloadQuota {
            mib: 2_048,
            bytes: 2_048 * MIB_BYTES,
        }
    );
    assert!(parse_download_quota_mib("1.5").is_err());
    assert!(parse_download_quota_mib("-1").is_err());
    assert!(parse_download_quota_mib("0").is_err());
    assert!(
        parse_download_quota_mib(&(cutlass_settings::MAX_DOWNLOAD_QUOTA_MIB + 1).to_string())
            .is_err()
    );
    assert!(
        parse_download_quota_mib(&cutlass_settings::MAX_DOWNLOAD_QUOTA_MIB.to_string()).is_ok()
    );
}

#[test]
fn cache_generation_is_monotonic_and_exhaustion_is_bounded() {
    let generation = AtomicU64::new(0);
    assert_eq!(next_cache_generation(&generation), Ok(1));
    assert_eq!(next_cache_generation(&generation), Ok(2));

    let exhausted = AtomicU64::new(u64::MAX - 1);
    let error = next_cache_generation(&exhausted).unwrap_err();
    assert_eq!(exhausted.load(Ordering::Acquire), u64::MAX);
    assert_eq!(error, CACHE_GENERATION_EXHAUSTED);
    assert!(error.chars().count() < MAX_CACHE_UI_ERROR_CHARS);
    assert_eq!(
        next_cache_generation(&exhausted),
        Err(CACHE_GENERATION_EXHAUSTED)
    );
}
