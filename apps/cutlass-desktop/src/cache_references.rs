//! Read-only discovery of project paths that point into persistent disk caches.
//!
//! Classification is purely lexical: it never canonicalizes a reference or
//! touches the referenced asset. Saved-draft discovery is shallow and refuses
//! symlink directories and symlink project files. Any ambiguity, malformed
//! reference, or exhausted bound makes the resulting inventory incomplete so a
//! destructive caller can fail closed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use cutlass_models::{ClipSource, Generator, Project};
use cutlass_storage::{CacheId, StorageLayout};

const PROJECT_FILE: &str = "project.cutlass";

/// Caches whose files can be persisted by a project.
pub(crate) const REFERENCE_CACHE_IDS: [CacheId; 4] = [
    CacheId::Download,
    CacheId::Luts,
    CacheId::Lottie,
    CacheId::Templates,
];

/// Maximum path-bearing project fields examined by one inventory.
pub(crate) const MAX_REFERENCES_EXAMINED: usize = 100_000;

/// Maximum child entries inspected beneath a caller-supplied drafts root.
pub(crate) const MAX_DRAFTS_SCANNED: usize = 10_000;

/// Maximum combined logical size of draft project files admitted to the loader.
pub(crate) const MAX_TOTAL_PROJECT_BYTES: u64 = 256 * 1024 * 1024;

/// Bounded accounting for path-bearing fields in one or more projects.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) struct ReferenceCounts {
    /// Media paths, LUT paths, and Lottie paths actually classified.
    pub(crate) examined: usize,
    /// Structurally unsafe or ambiguous paths.
    ///
    /// Absolute paths outside all managed roots are safely classified as
    /// external and do not increment this count.
    pub(crate) rejected: usize,
    /// More path-bearing fields existed than the configured examination cap.
    pub(crate) limit_reached: bool,
}

/// Deterministic, de-duplicated references grouped by their owning cache.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub(crate) struct CacheReferenceReport {
    /// Every key in [`REFERENCE_CACHE_IDS`] is present, including empty groups.
    pub(crate) by_cache: BTreeMap<CacheId, BTreeSet<PathBuf>>,
    pub(crate) counts: ReferenceCounts,
    /// False when a target root is missing, malformed, or overlaps another
    /// target root. No paths are returned in that state.
    pub(crate) cache_roots_valid: bool,
}

impl Default for CacheReferenceReport {
    fn default() -> Self {
        Self {
            by_cache: empty_reference_groups(),
            counts: ReferenceCounts::default(),
            cache_roots_valid: true,
        }
    }
}

impl CacheReferenceReport {
    /// Whether every path-bearing field was classified without ambiguity.
    pub(crate) fn is_complete(&self) -> bool {
        self.cache_roots_valid && self.counts.rejected == 0 && !self.counts.limit_reached
    }
}

/// Bounded accounting and references discovered from saved drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub(crate) struct DraftReferenceReport {
    /// Child entries inspected as possible draft directories.
    pub(crate) draft_entries_examined: usize,
    /// Valid `project.cutlass` files loaded.
    pub(crate) projects_loaded: usize,
    /// Logical bytes admitted to the model loader, including malformed files.
    pub(crate) project_bytes_read: u64,
    /// Known entries or project files skipped or rejected during discovery.
    ///
    /// When a limit flag is set, additional uncounted entries may remain.
    pub(crate) skipped_or_errored: usize,
    /// The draft-entry cap stopped discovery.
    pub(crate) draft_limit_reached: bool,
    /// At least one project file was skipped to preserve the byte cap.
    pub(crate) byte_limit_reached: bool,
    /// Aggregate, globally de-duplicated paths from all loaded projects.
    pub(crate) references: CacheReferenceReport,
}

impl Default for DraftReferenceReport {
    fn default() -> Self {
        Self {
            draft_entries_examined: 0,
            projects_loaded: 0,
            project_bytes_read: 0,
            skipped_or_errored: 0,
            draft_limit_reached: false,
            byte_limit_reached: false,
            references: CacheReferenceReport::default(),
        }
    }
}

impl DraftReferenceReport {
    /// Whether every discoverable draft and persisted cache reference was
    /// classified, making the inventory suitable for destructive decisions.
    pub(crate) fn is_complete(&self) -> bool {
        self.skipped_or_errored == 0
            && !self.draft_limit_reached
            && !self.byte_limit_reached
            && self.references.is_complete()
    }
}

#[derive(Debug, Clone, Copy)]
struct ScanLimits {
    draft_entries: usize,
    project_bytes: u64,
    references: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheRoots {
    entries: [(CacheId, PathBuf); 4],
}

impl CacheRoots {
    fn from_layout(layout: &StorageLayout) -> Result<Self, ()> {
        Self::from_paths([
            layout.resolve(CacheId::Download).ok_or(())?,
            layout.resolve(CacheId::Luts).ok_or(())?,
            layout.resolve(CacheId::Lottie).ok_or(())?,
            layout.resolve(CacheId::Templates).ok_or(())?,
        ])
    }

    fn from_paths(paths: [PathBuf; 4]) -> Result<Self, ()> {
        let [download, luts, lottie, templates] = paths;
        let roots = Self {
            entries: [
                (CacheId::Download, download),
                (CacheId::Luts, luts),
                (CacheId::Lottie, lottie),
                (CacheId::Templates, templates),
            ],
        };

        if roots
            .entries
            .iter()
            .any(|(_, root)| !is_valid_cache_root(root))
        {
            return Err(());
        }
        for index in 0..roots.entries.len() {
            let left = &roots.entries[index].1;
            for (_, right) in roots.entries.iter().skip(index + 1) {
                if paths_overlap(left, right) {
                    return Err(());
                }
            }
        }
        Ok(roots)
    }

    fn classify(&self, path: &Path) -> PathClassification {
        if !is_structurally_safe_absolute(path) {
            return PathClassification::Rejected;
        }
        if self.entries.iter().any(|(_, root)| path == root) {
            return PathClassification::Rejected;
        }

        let mut matched = None;
        for (id, root) in &self.entries {
            if is_lexically_beneath(root, path) {
                if matched.is_some() {
                    return PathClassification::Rejected;
                }
                matched = Some(*id);
            }
        }
        matched.map_or(PathClassification::External, PathClassification::Cache)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathClassification {
    Cache(CacheId),
    External,
    Rejected,
}

struct ReferenceAccumulator {
    by_cache: BTreeMap<CacheId, BTreeSet<PathBuf>>,
    counts: ReferenceCounts,
    limit: usize,
}

impl ReferenceAccumulator {
    fn new(limit: usize) -> Self {
        Self {
            by_cache: empty_reference_groups(),
            counts: ReferenceCounts::default(),
            limit,
        }
    }

    /// Returns false once no more references may be examined.
    fn consider(&mut self, roots: &CacheRoots, path: &Path) -> bool {
        if self.counts.examined >= self.limit {
            self.counts.limit_reached = true;
            return false;
        }
        self.counts.examined += 1;

        match roots.classify(path) {
            PathClassification::Cache(id) => {
                let Some(paths) = self.by_cache.get_mut(&id) else {
                    self.counts.rejected = self.counts.rejected.saturating_add(1);
                    return true;
                };
                paths.insert(path.to_path_buf());
            }
            PathClassification::External => {}
            PathClassification::Rejected => {
                self.counts.rejected = self.counts.rejected.saturating_add(1);
            }
        }
        true
    }

    fn into_report(self) -> CacheReferenceReport {
        CacheReferenceReport {
            by_cache: self.by_cache,
            counts: self.counts,
            cache_roots_valid: true,
        }
    }
}

/// Analyze every persisted path-bearing field without mutating the project.
///
/// Media-pool paths, every clip LUT, and every Lottie generator are classified
/// against all four managed roots. Absolute paths outside those roots are
/// intentionally ignored because they are not endangered by clearing a managed
/// cache.
pub(crate) fn analyze_project_references(
    project: &Project,
    layout: &StorageLayout,
) -> CacheReferenceReport {
    analyze_project_references_with_limit(project, layout, MAX_REFERENCES_EXAMINED)
}

fn analyze_project_references_with_limit(
    project: &Project,
    layout: &StorageLayout,
    limit: usize,
) -> CacheReferenceReport {
    analyze_project_with_roots(project, CacheRoots::from_layout(layout), limit)
}

fn analyze_project_with_roots(
    project: &Project,
    roots: Result<CacheRoots, ()>,
    limit: usize,
) -> CacheReferenceReport {
    let Ok(roots) = roots else {
        return CacheReferenceReport {
            cache_roots_valid: false,
            ..CacheReferenceReport::default()
        };
    };
    let mut references = ReferenceAccumulator::new(limit);
    analyze_project_into(project, &roots, &mut references);
    references.into_report()
}

/// Scan real draft directories immediately beneath `drafts_root`.
///
/// Missing roots are a complete empty inventory. The root, draft directories,
/// and `project.cutlass` files are inspected without following symlinks.
pub(crate) fn scan_saved_draft_references(
    drafts_root: &Path,
    layout: &StorageLayout,
) -> DraftReferenceReport {
    scan_saved_draft_references_with_limits(
        drafts_root,
        layout,
        ScanLimits {
            draft_entries: MAX_DRAFTS_SCANNED,
            project_bytes: MAX_TOTAL_PROJECT_BYTES,
            references: MAX_REFERENCES_EXAMINED,
        },
    )
}

fn scan_saved_draft_references_with_limits(
    drafts_root: &Path,
    layout: &StorageLayout,
    limits: ScanLimits,
) -> DraftReferenceReport {
    let mut report = DraftReferenceReport::default();
    let Ok(roots) = CacheRoots::from_layout(layout) else {
        report.references.cache_roots_valid = false;
        return report;
    };
    let mut references = ReferenceAccumulator::new(limits.references);

    match std::fs::symlink_metadata(drafts_root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            report.skipped_or_errored = 1;
            return report;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return report,
        Err(_) => {
            report.skipped_or_errored = 1;
            return report;
        }
    }

    let Ok(entries) = std::fs::read_dir(drafts_root) else {
        report.skipped_or_errored = 1;
        return report;
    };

    for (index, entry) in entries.enumerate() {
        if index >= limits.draft_entries {
            report.draft_limit_reached = true;
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            break;
        }
        report.draft_entries_examined = report.draft_entries_examined.saturating_add(1);

        let Ok(entry) = entry else {
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            continue;
        };
        // DirEntry::file_type does not follow a symlink. Path::is_dir would.
        if !file_type.is_dir() {
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            continue;
        }

        let project_path = entry.path().join(PROJECT_FILE);
        let Ok(metadata) = std::fs::symlink_metadata(&project_path) else {
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            continue;
        };
        if !metadata.file_type().is_file() {
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            continue;
        }

        let Some(next_bytes) = report.project_bytes_read.checked_add(metadata.len()) else {
            report.byte_limit_reached = true;
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            continue;
        };
        if next_bytes > limits.project_bytes {
            report.byte_limit_reached = true;
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            continue;
        }
        report.project_bytes_read = next_bytes;

        let Ok(project) = Project::load_from_file(&project_path) else {
            report.skipped_or_errored = report.skipped_or_errored.saturating_add(1);
            continue;
        };
        report.projects_loaded = report.projects_loaded.saturating_add(1);
        analyze_project_into(&project, &roots, &mut references);
        if references.counts.limit_reached {
            break;
        }
    }

    report.references = references.into_report();
    report
}

fn analyze_project_into(
    project: &Project,
    roots: &CacheRoots,
    references: &mut ReferenceAccumulator,
) {
    // The media pool is hash-backed. Sorting makes cap behavior deterministic,
    // not just the final BTreeSet ordering.
    let mut media = project.media_iter().collect::<Vec<_>>();
    media.sort_unstable_by(|left, right| {
        left.path()
            .cmp(right.path())
            .then_with(|| left.id.cmp(&right.id))
    });
    for source in media {
        if !references.consider(roots, source.path()) {
            return;
        }
    }

    for track in project.timeline().tracks_ordered() {
        for clip in track.clips_ordered() {
            if let Some(lut) = &clip.lut
                && !references.consider(roots, Path::new(&lut.path))
            {
                return;
            }
            if let ClipSource::Generated(Generator::Lottie { path, .. }) = &clip.content
                && !references.consider(roots, Path::new(path))
            {
                return;
            }
        }
    }
}

fn empty_reference_groups() -> BTreeMap<CacheId, BTreeSet<PathBuf>> {
    REFERENCE_CACHE_IDS
        .into_iter()
        .map(|id| (id, BTreeSet::new()))
        .collect()
}

fn is_valid_cache_root(path: &Path) -> bool {
    is_structurally_safe_absolute(path)
        && path
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
}

fn is_structurally_safe_absolute(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.is_absolute()
        && !path.as_os_str().as_encoded_bytes().contains(&0)
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn is_lexically_beneath(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    !relative.as_os_str().is_empty()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use cutlass_models::{Generator, Lut, MediaSource, Rational, TimeRange, TrackKind};
    use tempfile::TempDir;

    use super::*;

    const RATE: Rational = Rational::FPS_24;

    fn layout_in(temp: &TempDir) -> StorageLayout {
        StorageLayout::new(temp.path().join("storage")).unwrap()
    }

    fn cache_path(layout: &StorageLayout, id: CacheId, child: &str) -> PathBuf {
        layout.resolve(id).unwrap().join(child)
    }

    fn media(path: impl Into<PathBuf>) -> MediaSource {
        MediaSource::new(path, 1_920, 1_080, RATE, 240, true)
    }

    fn add_lottie(
        project: &mut Project,
        track: cutlass_models::TrackId,
        path: impl Into<String>,
        start: i64,
    ) -> cutlass_models::ClipId {
        project
            .add_generated(
                track,
                Generator::lottie(path, 512, 512),
                TimeRange::at_rate(start, 24, RATE),
            )
            .unwrap()
    }

    fn save_draft(root: &Path, name: &str, project: &Project) -> PathBuf {
        let directory = root.join(name);
        fs::create_dir(&directory).unwrap();
        let path = directory.join(PROJECT_FILE);
        project.save_to_file(&path).unwrap();
        path
    }

    fn singleton(path: PathBuf) -> BTreeSet<PathBuf> {
        BTreeSet::from([path])
    }

    #[test]
    fn media_paths_are_classified_against_all_four_roots() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let download = cache_path(&layout, CacheId::Download, "z/video.mp4");
        let lut = cache_path(&layout, CacheId::Luts, "b/look.cube");
        let lottie = cache_path(&layout, CacheId::Lottie, "a/sticker.json");
        let template = cache_path(&layout, CacheId::Templates, "c/intro.cutlass-template");
        let mut project = Project::new("all roots", RATE);
        for path in [&template, &download, &lottie, &lut] {
            project.add_media(media(path));
        }

        let report = analyze_project_references(&project, &layout);

        assert_eq!(report.counts.examined, 4);
        assert_eq!(report.counts.rejected, 0);
        assert!(report.is_complete());
        assert_eq!(
            report.by_cache.keys().copied().collect::<Vec<_>>(),
            REFERENCE_CACHE_IDS
        );
        assert_eq!(report.by_cache[&CacheId::Download], singleton(download));
        assert_eq!(report.by_cache[&CacheId::Luts], singleton(lut));
        assert_eq!(report.by_cache[&CacheId::Lottie], singleton(lottie));
        assert_eq!(report.by_cache[&CacheId::Templates], singleton(template));
    }

    #[test]
    fn clip_luts_and_lotties_are_deduplicated() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let lut = cache_path(&layout, CacheId::Luts, "cinema.cube");
        let lottie = cache_path(&layout, CacheId::Lottie, "confetti.json");
        let mut project = Project::new("clip references", RATE);
        let track = project.add_track(TrackKind::Sticker, "Stickers");

        for start in [0, 24] {
            let clip = add_lottie(
                &mut project,
                track,
                lottie.to_string_lossy().into_owned(),
                start,
            );
            project
                .set_clip_lut(clip, Some(Lut::new(lut.to_string_lossy().into_owned())))
                .unwrap();
        }

        let report = analyze_project_references(&project, &layout);

        assert_eq!(
            report.counts,
            ReferenceCounts {
                examined: 4,
                rejected: 0,
                limit_reached: false,
            }
        );
        assert_eq!(report.by_cache[&CacheId::Luts], singleton(lut));
        assert_eq!(report.by_cache[&CacheId::Lottie], singleton(lottie));
        assert!(report.is_complete());
    }

    #[test]
    fn unsafe_paths_are_rejected_while_absolute_outside_paths_are_external() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let download_root = layout.resolve(CacheId::Download).unwrap();
        let valid = download_root.join("valid.mp4");
        let outside = temp.path().join("outside.mp4");
        let traversal = download_root.join("nested/../../escape.mp4");
        let mut malformed_media = download_root.join("malformed");
        malformed_media.as_mut_os_string().push("\0.mp4");

        let mut project = Project::new("unsafe", RATE);
        for path in [
            valid.clone(),
            outside,
            PathBuf::from("relative.mp4"),
            download_root,
            traversal,
            malformed_media,
        ] {
            project.add_media(media(path));
        }

        let lottie_root = layout.resolve(CacheId::Lottie).unwrap();
        let malformed_lottie = format!("{}/bad\0.json", lottie_root.display());
        let track = project.add_track(TrackKind::Sticker, "Stickers");
        let clip = add_lottie(&mut project, track, malformed_lottie, 0);
        project
            .set_clip_lut(clip, Some(Lut::new("relative.cube")))
            .unwrap();

        let report = analyze_project_references(&project, &layout);

        assert_eq!(report.counts.examined, 8);
        assert_eq!(report.counts.rejected, 6);
        assert_eq!(report.by_cache[&CacheId::Download], singleton(valid));
        assert!(!report.is_complete());
    }

    #[test]
    fn overlapping_cache_roots_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("managed");
        let roots = CacheRoots::from_paths([
            parent.clone(),
            parent.join("nested"),
            temp.path().join("lottie"),
            temp.path().join("templates"),
        ]);
        let project = Project::new("overlap", RATE);

        let report = analyze_project_with_roots(&project, roots, 10);

        assert!(!report.cache_roots_valid);
        assert_eq!(report.counts, ReferenceCounts::default());
        assert!(report.by_cache.values().all(BTreeSet::is_empty));
        assert!(!report.is_complete());
    }

    #[test]
    fn reference_cap_is_bounded_and_deterministic() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let root = layout.resolve(CacheId::Download).unwrap();
        let a = root.join("a.mp4");
        let m = root.join("m.mp4");
        let z = root.join("z.mp4");
        let mut project = Project::new("limit", RATE);
        for path in [&z, &a, &m] {
            project.add_media(media(path));
        }

        let report = analyze_project_references_with_limit(&project, &layout, 2);

        assert_eq!(
            report.counts,
            ReferenceCounts {
                examined: 2,
                rejected: 0,
                limit_reached: true,
            }
        );
        assert_eq!(report.by_cache[&CacheId::Download], BTreeSet::from([a, m]));
        assert!(!report.is_complete());
    }

    #[test]
    fn valid_saved_draft_scan_collects_every_reference_type() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let drafts = temp.path().join("projects");
        fs::create_dir(&drafts).unwrap();

        let download = cache_path(&layout, CacheId::Download, "media.mp4");
        let template = cache_path(&layout, CacheId::Templates, "sample.mp4");
        let lut = cache_path(&layout, CacheId::Luts, "look.cube");
        let lottie = cache_path(&layout, CacheId::Lottie, "motion.json");
        let mut project = Project::new("saved", RATE);
        project.add_media(media(&download));
        project.add_media(media(&template));
        let track = project.add_track(TrackKind::Sticker, "Stickers");
        let clip = add_lottie(
            &mut project,
            track,
            lottie.to_string_lossy().into_owned(),
            0,
        );
        project
            .set_clip_lut(clip, Some(Lut::new(lut.to_string_lossy().into_owned())))
            .unwrap();
        save_draft(&drafts, "valid", &project);

        let report = scan_saved_draft_references(&drafts, &layout);

        assert_eq!(report.draft_entries_examined, 1);
        assert_eq!(report.projects_loaded, 1);
        assert_eq!(report.skipped_or_errored, 0);
        assert_eq!(report.references.counts.examined, 4);
        assert_eq!(
            report.references.by_cache[&CacheId::Download],
            singleton(download)
        );
        assert_eq!(
            report.references.by_cache[&CacheId::Templates],
            singleton(template)
        );
        assert_eq!(report.references.by_cache[&CacheId::Luts], singleton(lut));
        assert_eq!(
            report.references.by_cache[&CacheId::Lottie],
            singleton(lottie)
        );
        assert!(report.is_complete());
    }

    #[test]
    fn draft_scan_continues_past_malformed_and_missing_projects() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let drafts = temp.path().join("projects");
        fs::create_dir(&drafts).unwrap();

        let referenced = cache_path(&layout, CacheId::Download, "kept.mp4");
        let mut project = Project::new("valid", RATE);
        project.add_media(media(&referenced));
        let valid_path = save_draft(&drafts, "valid", &project);
        let valid_bytes = fs::metadata(valid_path).unwrap().len();

        let malformed_dir = drafts.join("malformed");
        fs::create_dir(&malformed_dir).unwrap();
        let malformed = b"{broken";
        fs::write(malformed_dir.join(PROJECT_FILE), malformed).unwrap();
        fs::create_dir(drafts.join("missing-project")).unwrap();
        fs::write(drafts.join("not-a-draft"), b"ignored").unwrap();

        let report = scan_saved_draft_references(&drafts, &layout);

        assert_eq!(report.draft_entries_examined, 4);
        assert_eq!(report.projects_loaded, 1);
        assert_eq!(
            report.project_bytes_read,
            valid_bytes + malformed.len() as u64
        );
        assert_eq!(report.skipped_or_errored, 3);
        assert_eq!(
            report.references.by_cache[&CacheId::Download],
            singleton(referenced)
        );
        assert!(!report.draft_limit_reached);
        assert!(!report.byte_limit_reached);
        assert!(!report.is_complete());
    }

    #[test]
    fn missing_drafts_root_is_a_complete_empty_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);

        let report = scan_saved_draft_references(&temp.path().join("missing-projects"), &layout);

        assert_eq!(report, DraftReferenceReport::default());
        assert!(report.is_complete());
    }

    #[test]
    fn draft_entry_cap_stops_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let drafts = temp.path().join("projects");
        fs::create_dir(&drafts).unwrap();

        for index in 0..3 {
            let path = cache_path(&layout, CacheId::Download, &format!("media-{index}.mp4"));
            let mut project = Project::new(format!("draft {index}"), RATE);
            project.add_media(media(path));
            save_draft(&drafts, &format!("draft-{index}"), &project);
        }

        let report = scan_saved_draft_references_with_limits(
            &drafts,
            &layout,
            ScanLimits {
                draft_entries: 2,
                project_bytes: u64::MAX,
                references: usize::MAX,
            },
        );

        assert_eq!(report.draft_entries_examined, 2);
        assert_eq!(report.projects_loaded, 2);
        assert_eq!(report.skipped_or_errored, 1);
        assert!(report.draft_limit_reached);
        assert!(!report.byte_limit_reached);
        assert_eq!(report.references.counts.examined, 2);
        assert!(!report.is_complete());
    }

    #[test]
    fn total_project_byte_cap_skips_files_that_do_not_fit() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let drafts = temp.path().join("projects");
        fs::create_dir(&drafts).unwrap();

        let path = cache_path(&layout, CacheId::Download, "shared.mp4");
        let mut project = Project::new("source", RATE);
        project.add_media(media(path));
        let source = temp.path().join("source.cutlass");
        project.save_to_file(&source).unwrap();
        let bytes = fs::read(&source).unwrap();
        for name in ["first", "second"] {
            let directory = drafts.join(name);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join(PROJECT_FILE), &bytes).unwrap();
        }

        let report = scan_saved_draft_references_with_limits(
            &drafts,
            &layout,
            ScanLimits {
                draft_entries: 10,
                project_bytes: bytes.len() as u64,
                references: usize::MAX,
            },
        );

        assert_eq!(report.draft_entries_examined, 2);
        assert_eq!(report.projects_loaded, 1);
        assert_eq!(report.project_bytes_read, bytes.len() as u64);
        assert_eq!(report.skipped_or_errored, 1);
        assert!(!report.draft_limit_reached);
        assert!(report.byte_limit_reached);
        assert_eq!(report.references.counts.examined, 1);
        assert!(!report.is_complete());
    }

    #[test]
    fn saved_draft_reference_cap_is_global() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let drafts = temp.path().join("projects");
        fs::create_dir(&drafts).unwrap();
        let root = layout.resolve(CacheId::Download).unwrap();
        let mut project = Project::new("many references", RATE);
        for name in ["c.mp4", "a.mp4", "b.mp4"] {
            project.add_media(media(root.join(name)));
        }
        save_draft(&drafts, "only", &project);

        let report = scan_saved_draft_references_with_limits(
            &drafts,
            &layout,
            ScanLimits {
                draft_entries: 10,
                project_bytes: u64::MAX,
                references: 2,
            },
        );

        assert_eq!(report.projects_loaded, 1);
        assert_eq!(report.references.counts.examined, 2);
        assert!(report.references.counts.limit_reached);
        assert_eq!(
            report.references.by_cache[&CacheId::Download],
            BTreeSet::from([root.join("a.mp4"), root.join("b.mp4")])
        );
        assert!(!report.is_complete());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_draft_directories_are_not_traversed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let layout = layout_in(&temp);
        let drafts = temp.path().join("projects");
        fs::create_dir(&drafts).unwrap();

        let mut project = Project::new("linked", RATE);
        project.add_media(media(cache_path(&layout, CacheId::Download, "linked.mp4")));
        project
            .save_to_file(&outside.path().join(PROJECT_FILE))
            .unwrap();
        symlink(outside.path(), drafts.join("linked-draft")).unwrap();

        let report = scan_saved_draft_references(&drafts, &layout);

        assert_eq!(report.draft_entries_examined, 1);
        assert_eq!(report.projects_loaded, 0);
        assert_eq!(report.skipped_or_errored, 1);
        assert_eq!(report.references.counts, ReferenceCounts::default());
        assert!(report.references.by_cache.values().all(BTreeSet::is_empty));
        assert!(!report.is_complete());
    }
}
