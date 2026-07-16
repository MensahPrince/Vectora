use super::*;
use cutlass_cloud::dto::AssetKind;
use cutlass_storage::StorageLayout;
use std::fs;

fn entry(duration: Option<f64>, slots: Option<u32>) -> CatalogEntry {
    CatalogEntry {
        id: "tpl-1".into(),
        kind: AssetKind::Template,
        name: "T".into(),
        category: "vlog".into(),
        tags: vec![],
        file_url: String::new(),
        preview_url: None,
        size_bytes: 0,
        checksum_sha256: String::new(),
        min_schema_version: None,
        author: "Cutlass".into(),
        license: "CC0".into(),
        duration_seconds: duration,
        slot_count: slots,
    }
}

fn template_layout(templates_root: &Path) -> SharedStorageLayout {
    let mut layout = StorageLayout::new(templates_root.parent().unwrap().join("default")).unwrap();
    layout
        .set_override(CacheId::Templates, templates_root)
        .unwrap();
    SharedStorageLayout::new(layout)
}

fn write_installed_template(templates_root: &Path, id: &str) -> PathBuf {
    let install_dir = templates_root.join(id);
    fs::create_dir_all(&install_dir).unwrap();
    let template_path = install_dir.join(INSTALLED_TEMPLATE_FILE);
    fs::write(&template_path, b"template").unwrap();
    template_path
}

#[test]
fn tile_labels() {
    assert_eq!(tile_label(&entry(Some(72.4), Some(3))), "1:12 · 3 clips");
    assert_eq!(tile_label(&entry(Some(9.0), Some(1))), "0:09 · 1 clip");
    assert_eq!(tile_label(&entry(None, None)), "");
}

#[test]
fn safe_id_strips_traversal_material() {
    assert_eq!(safe_id("tpl-vlog-1"), "tpl-vlog-1");
    assert_eq!(safe_id("../../etc/passwd"), "etcpasswd");
    assert_eq!(safe_id("..."), "unnamed");
}

#[test]
fn strict_installed_template_resolution_returns_the_current_absolute_path() {
    let dir = tempfile::tempdir().unwrap();
    let templates_root = dir.path().join("templates");
    let template_path = write_installed_template(&templates_root, "tpl-vlog_1.2");
    let layout = template_layout(&templates_root);
    let lease = layout.lease();

    assert!(template_path.is_absolute());
    assert_eq!(
        resolve_installed_template_path(&lease, "tpl-vlog_1.2").unwrap(),
        template_path
    );
}

#[test]
fn strict_template_ids_reject_malformed_paths_traversal_and_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let templates_root = dir.path().join("templates");
    let template_path = write_installed_template(&templates_root, "tpl-vlog-1");
    write_installed_template(&templates_root, "unnamed");
    let layout = template_layout(&templates_root);
    let lease = layout.lease();

    let invalid = [
        "",
        "unnamed",
        ".",
        "..",
        "../tpl-vlog-1",
        ".tpl-vlog-1.",
        "tpl-vlog-1/",
        "tpl/vlog-1",
        "tpl\\vlog-1",
        "tpl-vlog-1/../other",
        "TPL-VLOG-1",
        "Tpl-vlog-1",
        "tpl vlog-1",
        "tpl-vlog-💥",
    ];
    for id in invalid {
        assert!(
            InstalledTemplateId::from_untrusted(id).is_err(),
            "strict parser accepted {id:?}"
        );
        assert!(
            resolve_installed_template_path(&lease, id).is_err(),
            "resolver accepted {id:?}"
        );
    }

    let full_path = template_path.to_string_lossy();
    assert!(InstalledTemplateId::from_untrusted(&full_path).is_err());
    assert!(resolve_installed_template_path(&lease, &full_path).is_err());

    let too_long = "a".repeat(MAX_UNTRUSTED_TEMPLATE_ID_BYTES + 1);
    assert!(InstalledTemplateId::from_untrusted(&too_long).is_err());
    assert!(resolve_installed_template_path(&lease, &too_long).is_err());
}

#[test]
fn installed_template_resolution_rejects_missing_install_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let templates_root = dir.path().join("templates");
    fs::create_dir(&templates_root).unwrap();
    let layout = template_layout(&templates_root);
    let lease = layout.lease();

    assert!(resolve_installed_template_path(&lease, "missing").is_err());

    fs::create_dir(templates_root.join("missing")).unwrap();
    assert!(resolve_installed_template_path(&lease, "missing").is_err());
}

#[test]
fn installed_template_resolution_rejects_directory_at_template_file() {
    let dir = tempfile::tempdir().unwrap();
    let templates_root = dir.path().join("templates");
    let install_dir = templates_root.join("tpl-vlog-1");
    fs::create_dir_all(install_dir.join(INSTALLED_TEMPLATE_FILE)).unwrap();
    let layout = template_layout(&templates_root);
    let lease = layout.lease();

    assert!(resolve_installed_template_path(&lease, "tpl-vlog-1").is_err());
}

#[cfg(unix)]
#[test]
fn installed_template_resolution_refuses_symlink_install_directory() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let templates_root = dir.path().join("templates");
    fs::create_dir(&templates_root).unwrap();
    let outside = dir.path().join("outside-install");
    let outside_template = write_installed_template(dir.path(), "outside-install");
    symlink(&outside, templates_root.join("tpl-vlog-1")).unwrap();
    let layout = template_layout(&templates_root);
    let lease = layout.lease();

    assert!(resolve_installed_template_path(&lease, "tpl-vlog-1").is_err());
    assert_eq!(fs::read(outside_template).unwrap(), b"template");
}

#[cfg(unix)]
#[test]
fn installed_template_resolution_refuses_symlink_template_file() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let templates_root = dir.path().join("templates");
    let install_dir = templates_root.join("tpl-vlog-1");
    fs::create_dir_all(&install_dir).unwrap();
    let outside = dir.path().join("outside.cutlasst");
    fs::write(&outside, b"outside").unwrap();
    symlink(&outside, install_dir.join(INSTALLED_TEMPLATE_FILE)).unwrap();
    let layout = template_layout(&templates_root);
    let lease = layout.lease();

    assert!(resolve_installed_template_path(&lease, "tpl-vlog-1").is_err());
    assert_eq!(fs::read(outside).unwrap(), b"outside");
}

#[cfg(windows)]
#[test]
fn windows_reparse_attribute_helper_rejects_all_reparse_points() {
    assert!(windows_attributes_are_reparse_point(
        WINDOWS_FILE_ATTRIBUTE_REPARSE_POINT
    ));
    assert!(windows_attributes_are_reparse_point(
        WINDOWS_FILE_ATTRIBUTE_REPARSE_POINT | 0x10
    ));
    assert!(!windows_attributes_are_reparse_point(0x10));
}

#[test]
fn deferred_apply_rebuilds_path_from_the_post_picker_generation() {
    let dir = tempfile::tempdir().unwrap();
    let first_root = dir.path().join("templates-a");
    let second_root = dir.path().join("templates-b");
    let template_id = InstalledTemplateId::from_catalog_id("../../tpl-vlog-1");
    assert_eq!(template_id.as_str(), "tpl-vlog-1");

    let mut first = StorageLayout::new(dir.path().join("default-a")).unwrap();
    first.set_override(CacheId::Templates, &first_root).unwrap();
    let layout = SharedStorageLayout::new(first);

    let first_lease = layout.lease();
    let first_generation = first_lease.generation();
    let (first_install_dir, first_template_path) =
        installed_template_paths(&first_lease, &template_id).unwrap();
    assert_eq!(first_install_dir, first_root.join("tpl-vlog-1"));
    assert_eq!(
        first_template_path,
        first_root.join("tpl-vlog-1").join(INSTALLED_TEMPLATE_FILE)
    );
    drop(first_lease);

    let mut second = StorageLayout::new(dir.path().join("default-b")).unwrap();
    second
        .set_override(CacheId::Templates, &second_root)
        .unwrap();
    layout.replace(first_generation, second).unwrap();

    let second_lease = layout.lease();
    assert_eq!(second_lease.generation(), first_generation + 1);
    let (second_install_dir, second_template_path) =
        installed_template_paths(&second_lease, &template_id).unwrap();
    assert_eq!(second_install_dir, second_root.join("tpl-vlog-1"));
    assert_eq!(
        second_template_path,
        second_root.join("tpl-vlog-1").join(INSTALLED_TEMPLATE_FILE)
    );
    assert_ne!(first_template_path, second_template_path);
}

#[test]
fn operations_use_overrides_then_pick_up_the_next_generation() {
    let dir = tempfile::tempdir().unwrap();
    let first_catalog = dir.path().join("catalog-a");
    let first_templates = dir.path().join("templates-a");
    let second_catalog = dir.path().join("catalog-b");
    let second_templates = dir.path().join("templates-b");

    let mut first = StorageLayout::new(dir.path().join("default-a")).unwrap();
    first
        .set_override(CacheId::Catalog, &first_catalog)
        .unwrap();
    first
        .set_override(CacheId::Templates, &first_templates)
        .unwrap();
    let layout = SharedStorageLayout::new(first);

    let first_refresh_lease = layout.lease();
    let first_generation = first_refresh_lease.generation();
    let first_refresh = catalog_root(&first_refresh_lease).unwrap();
    drop(first_refresh_lease);

    let first_install_lease = layout.lease();
    assert_eq!(first_install_lease.generation(), first_generation);
    let first_install = templates_root(&first_install_lease).unwrap();
    drop(first_install_lease);

    assert_eq!(first_refresh, first_catalog);
    assert_eq!(first_install, first_templates);

    let mut second = StorageLayout::new(dir.path().join("default-b")).unwrap();
    second
        .set_override(CacheId::Catalog, &second_catalog)
        .unwrap();
    second
        .set_override(CacheId::Templates, &second_templates)
        .unwrap();
    layout.replace(first_generation, second).unwrap();

    assert_eq!(first_refresh, first_catalog);
    assert_eq!(first_install, first_templates);

    let second_refresh_lease = layout.lease();
    assert_eq!(second_refresh_lease.generation(), first_generation + 1);
    assert_eq!(catalog_root(&second_refresh_lease).unwrap(), second_catalog);
    drop(second_refresh_lease);

    let second_install_lease = layout.lease();
    assert_eq!(second_install_lease.generation(), first_generation + 1);
    assert_eq!(
        templates_root(&second_install_lease).unwrap(),
        second_templates
    );
}
