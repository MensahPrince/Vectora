//! Absolute storage roots and shared, versioned layouts.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::cache_registry::{CacheId, CacheKind, cache_descriptors};
use crate::error::StorageError;
use crate::ops::{normalize_absolute, paths_overlap, resolve_physical_cache_root};

/// Absolute storage root plus optional per-disk-cache path overrides.
///
/// Overrides are held in a [`BTreeMap`] so snapshots and diagnostics remain
/// deterministic regardless of insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageLayout {
    root: PathBuf,
    overrides: BTreeMap<CacheId, PathBuf>,
}

impl StorageLayout {
    /// Create a layout with no overrides.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = normalize_absolute(&root.into(), "storage root", false)?;
        Ok(Self {
            root,
            overrides: BTreeMap::new(),
        })
    }

    /// Create a layout from stable string keys and absolute paths.
    ///
    /// Unknown keys, memory-cache keys, relative paths, and duplicate keys
    /// are rejected rather than ignored.
    pub fn with_overrides<I, K, P>(
        root: impl Into<PathBuf>,
        overrides: I,
    ) -> Result<Self, StorageError>
    where
        I: IntoIterator<Item = (K, P)>,
        K: AsRef<str>,
        P: Into<PathBuf>,
    {
        let mut layout = Self::new(root)?;
        for (key, path) in overrides {
            let id = CacheId::parse(key.as_ref()).map_err(|_| StorageError::UnknownCacheId)?;
            if layout.overrides.contains_key(&id) {
                return Err(StorageError::DuplicateOverride(id));
            }
            layout.set_override(id, path)?;
        }
        Ok(layout)
    }

    /// Absolute default root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Deterministically ordered cache overrides.
    pub fn overrides(&self) -> &BTreeMap<CacheId, PathBuf> {
        &self.overrides
    }

    /// Return the override for one cache, if set.
    pub fn override_for(&self, id: CacheId) -> Option<&Path> {
        self.overrides.get(&id).map(PathBuf::as_path)
    }

    /// Set an absolute override for a disk cache.
    pub fn set_override(
        &mut self,
        id: CacheId,
        path: impl Into<PathBuf>,
    ) -> Result<Option<PathBuf>, StorageError> {
        if id.descriptor().kind != CacheKind::Disk {
            return Err(StorageError::CacheIsNotDisk(id));
        }
        let path = normalize_absolute(&path.into(), "cache override", false)?;
        self.ensure_override_is_disjoint(id, &path)?;
        Ok(self.overrides.insert(id, path))
    }

    /// Set an override using an exact stable cache key.
    pub fn set_override_key(
        &mut self,
        key: &str,
        path: impl Into<PathBuf>,
    ) -> Result<Option<PathBuf>, StorageError> {
        let id = CacheId::parse(key).map_err(|_| StorageError::UnknownCacheId)?;
        self.set_override(id, path)
    }

    /// Remove and return an override.
    pub fn remove_override(&mut self, id: CacheId) -> Option<PathBuf> {
        self.overrides.remove(&id)
    }

    /// Resolve a cache path.
    ///
    /// Memory caches return `None`. Disk caches use an override when present,
    /// otherwise `root/default_relative`.
    pub fn resolve(&self, id: CacheId) -> Option<PathBuf> {
        let descriptor = id.descriptor();
        if descriptor.kind == CacheKind::Memory {
            return None;
        }
        self.overrides.get(&id).cloned().or_else(|| {
            descriptor
                .default_relative
                .map(|relative| self.root.join(relative))
        })
    }

    /// Resolve every disk cache in deterministic registry order.
    pub fn resolved_disk_paths(&self) -> Vec<(CacheId, PathBuf)> {
        cache_descriptors()
            .iter()
            .filter_map(|descriptor| {
                self.resolve(descriptor.id)
                    .map(|path| (descriptor.id, path))
            })
            .collect()
    }

    /// Validate every resolved disk cache root against the current filesystem.
    ///
    /// Existing parent symlinks are resolved to compare physical locations.
    /// Existing cache roots must be real directories, while missing roots are
    /// allowed beneath the nearest existing directory. This check is read-only
    /// and never creates missing paths.
    ///
    /// This is a point-in-time validation, not a race-free filesystem
    /// transaction; filesystem entries can change after it returns.
    pub fn validate_filesystem(&self) -> Result<(), StorageError> {
        let mut physical_paths = Vec::new();
        for descriptor in cache_descriptors() {
            if descriptor.kind != CacheKind::Disk {
                continue;
            }
            let path = self
                .resolve(descriptor.id)
                .ok_or(StorageError::DangerousPath("disk cache has no path"))?;
            physical_paths.push((descriptor.id, resolve_physical_cache_root(&path)?));
        }

        for (index, (cache, path)) in physical_paths.iter().enumerate() {
            for (other, other_path) in &physical_paths[index + 1..] {
                if paths_overlap(path, other_path) {
                    return Err(StorageError::CachePathsOverlap {
                        cache: *cache,
                        other: *other,
                    });
                }
            }
        }
        Ok(())
    }

    fn ensure_override_is_disjoint(
        &self,
        id: CacheId,
        candidate: &Path,
    ) -> Result<(), StorageError> {
        for descriptor in cache_descriptors() {
            if descriptor.id == id || descriptor.kind != CacheKind::Disk {
                continue;
            }
            let default = self
                .root
                .join(descriptor.default_relative.expect("disk cache has a path"));
            if paths_overlap(candidate, &default)
                || self
                    .overrides
                    .get(&descriptor.id)
                    .is_some_and(|other| paths_overlap(candidate, other))
            {
                return Err(StorageError::CachePathsOverlap {
                    cache: id,
                    other: descriptor.id,
                });
            }
        }
        Ok(())
    }
}

/// An immutable, versioned copy of a [`SharedStorageLayout`].
///
/// The layout and generation are captured while holding one read lock, so
/// every path resolved through this snapshot belongs to the reported
/// generation. Mutating a layout returned by [`Self::into_parts`] cannot
/// affect the shared value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageLayoutSnapshot {
    layout: StorageLayout,
    generation: u64,
}

impl StorageLayoutSnapshot {
    /// Return the captured layout.
    pub fn layout(&self) -> &StorageLayout {
        &self.layout
    }

    /// Return the generation captured with the layout.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Resolve a cache against the captured layout.
    pub fn resolve(&self, id: CacheId) -> Option<PathBuf> {
        self.layout.resolve(id)
    }

    /// Consume the snapshot into its detached layout and generation.
    pub fn into_parts(self) -> (StorageLayout, u64) {
        (self.layout, self.generation)
    }
}

/// A borrowed layout generation held stable for one complete cache operation.
///
/// While this lease exists, a writer cannot publish a replacement layout.
/// Cache workers should acquire one at the start of an operation and retain it
/// until every filesystem access using its resolved paths has finished.
pub struct StorageLayoutLease<'a> {
    state: RwLockReadGuard<'a, VersionedStorageLayout>,
}

impl StorageLayoutLease<'_> {
    /// Return the coherently borrowed layout.
    pub fn layout(&self) -> &StorageLayout {
        &self.state.layout
    }

    /// Return the generation held by this lease.
    pub fn generation(&self) -> u64 {
        self.state.generation
    }

    /// Resolve a cache against the held generation.
    pub fn resolve(&self, id: CacheId) -> Option<PathBuf> {
        self.state.layout.resolve(id)
    }
}

/// A cheap-to-clone, thread-safe, versioned [`StorageLayout`].
///
/// Clones share one layout through an [`Arc`]. Readers observe the layout and
/// generation coherently, while writers use an expected generation to prevent
/// stale worker results from replacing newer configuration. The initial
/// generation is zero and each successful write increments it exactly once.
///
/// Lock poisoning is recovered internally. Write operations prepare a
/// complete replacement before committing it, so an unwinding update leaves
/// the prior layout and generation valid.
#[derive(Debug, Clone)]
pub struct SharedStorageLayout {
    inner: Arc<RwLock<VersionedStorageLayout>>,
}

#[derive(Debug)]
struct VersionedStorageLayout {
    layout: StorageLayout,
    generation: u64,
}

impl SharedStorageLayout {
    /// Create shared storage state at generation zero.
    ///
    /// The supplied layout has already been validated by [`StorageLayout`]'s
    /// constructors and mutation methods.
    pub fn new(layout: StorageLayout) -> Self {
        Self::from_generation(layout, 0)
    }

    /// Clone the current layout and generation under one coherent read.
    pub fn snapshot(&self) -> StorageLayoutSnapshot {
        let state = self.read_state();
        StorageLayoutSnapshot {
            layout: state.layout.clone(),
            generation: state.generation,
        }
    }

    /// Hold one generation stable for a complete cache operation.
    ///
    /// Keep the returned lease alive until all work using its paths finishes.
    /// Long-lived leases delay relocation, so callers should scope them to one
    /// refresh, download, install, render, or maintenance operation.
    pub fn lease(&self) -> StorageLayoutLease<'_> {
        StorageLayoutLease {
            state: self.read_state(),
        }
    }

    /// Resolve one cache from a single coherent read of the current layout.
    ///
    /// Memory caches return `None`.
    pub fn resolve(&self, id: CacheId) -> Option<PathBuf> {
        self.resolve_versioned(id).0
    }

    /// Resolve one cache together with the generation used for that path.
    ///
    /// The returned path and generation are captured under the same read lock.
    /// Memory caches return `None` for the path.
    pub fn resolve_versioned(&self, id: CacheId) -> (Option<PathBuf>, u64) {
        let state = self.read_state();
        (state.layout.resolve(id), state.generation)
    }

    /// Replace the layout when `expected_generation` is still current.
    ///
    /// The replacement must be a previously validated [`StorageLayout`].
    /// Success returns the newly installed generation. A stale generation or
    /// exhausted generation leaves the current layout untouched.
    pub fn replace(
        &self,
        expected_generation: u64,
        replacement: StorageLayout,
    ) -> Result<u64, SharedStorageLayoutError> {
        let mut state = self.write_state();
        let next_generation = checked_next_generation(&state, expected_generation)?;
        *state = VersionedStorageLayout {
            layout: replacement,
            generation: next_generation,
        };
        Ok(next_generation)
    }

    /// Run an exclusive transition and publish `replacement` on success.
    ///
    /// Existing [`StorageLayoutLease`] values finish before `transition`
    /// starts, and new leases wait until this method returns. The callback
    /// receives the old and replacement layouts while publication is
    /// exclusive. A callback error or panic leaves the shared layout and
    /// generation unchanged.
    pub fn transition<F, E>(
        &self,
        expected_generation: u64,
        replacement: StorageLayout,
        transition: F,
    ) -> Result<u64, SharedStorageLayoutTransitionError<E>>
    where
        F: FnOnce(&StorageLayout, &StorageLayout) -> Result<(), E>,
    {
        let mut state = self.write_state();
        let next_generation = checked_next_generation(&state, expected_generation)
            .map_err(SharedStorageLayoutTransitionError::Layout)?;
        transition(&state.layout, &replacement)
            .map_err(SharedStorageLayoutTransitionError::Transition)?;
        *state = VersionedStorageLayout {
            layout: replacement,
            generation: next_generation,
        };
        Ok(next_generation)
    }

    /// Update a cloned layout when `expected_generation` is still current.
    ///
    /// The callback runs with the write lock held and should be brief. It must
    /// not call methods on this shared value, which would deadlock. The clone
    /// begins valid, and [`StorageLayout`] mutation methods preserve that
    /// validity. The callback is responsible for handling their errors.
    ///
    /// A stale or exhausted generation refuses the update without invoking the
    /// callback. If the callback panics, the panic propagates, no state is
    /// committed, and subsequent operations recover the poisoned lock.
    pub fn update<F>(
        &self,
        expected_generation: u64,
        update: F,
    ) -> Result<u64, SharedStorageLayoutError>
    where
        F: FnOnce(&mut StorageLayout),
    {
        let mut state = self.write_state();
        let next_generation = checked_next_generation(&state, expected_generation)?;
        let mut replacement = state.layout.clone();
        update(&mut replacement);
        *state = VersionedStorageLayout {
            layout: replacement,
            generation: next_generation,
        };
        Ok(next_generation)
    }

    fn from_generation(layout: StorageLayout, generation: u64) -> Self {
        Self {
            inner: Arc::new(RwLock::new(VersionedStorageLayout { layout, generation })),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_generation_for_test(layout: StorageLayout, generation: u64) -> Self {
        Self::from_generation(layout, generation)
    }

    fn read_state(&self) -> RwLockReadGuard<'_, VersionedStorageLayout> {
        match self.inner.read() {
            Ok(state) => state,
            Err(poisoned) => {
                let state = poisoned.into_inner();
                self.inner.clear_poison();
                state
            }
        }
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, VersionedStorageLayout> {
        match self.inner.write() {
            Ok(state) => state,
            Err(poisoned) => {
                let state = poisoned.into_inner();
                self.inner.clear_poison();
                state
            }
        }
    }
}

impl From<StorageLayout> for SharedStorageLayout {
    fn from(layout: StorageLayout) -> Self {
        Self::new(layout)
    }
}

fn checked_next_generation(
    state: &VersionedStorageLayout,
    expected_generation: u64,
) -> Result<u64, SharedStorageLayoutError> {
    if state.generation != expected_generation {
        return Err(SharedStorageLayoutError::StaleGeneration {
            expected: expected_generation,
            current: state.generation,
        });
    }
    state
        .generation
        .checked_add(1)
        .ok_or(SharedStorageLayoutError::GenerationExhausted)
}

/// A rejected [`SharedStorageLayout`] write.
///
/// This error contains only generation numbers and never retains or displays
/// storage paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedStorageLayoutError {
    /// Another writer replaced the layout after the caller took its snapshot.
    StaleGeneration {
        /// Generation supplied by the caller.
        expected: u64,
        /// Generation currently installed.
        current: u64,
    },
    /// The current generation is `u64::MAX` and cannot advance safely.
    GenerationExhausted,
}

impl fmt::Display for SharedStorageLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration { expected, current } => write!(
                f,
                "stale storage layout generation: expected {expected}, current {current}"
            ),
            Self::GenerationExhausted => f.write_str("storage layout generation cannot advance"),
        }
    }
}

impl Error for SharedStorageLayoutError {}

/// A rejected exclusive [`SharedStorageLayout::transition`].
#[derive(Debug)]
pub enum SharedStorageLayoutTransitionError<E> {
    /// The expected generation was stale or exhausted.
    Layout(SharedStorageLayoutError),
    /// The caller's transition failed before publication.
    Transition(E),
}

impl<E: fmt::Display> fmt::Display for SharedStorageLayoutTransitionError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(error) => error.fmt(f),
            Self::Transition(error) => write!(f, "storage layout transition failed: {error}"),
        }
    }
}

impl<E> Error for SharedStorageLayoutTransitionError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Layout(error) => Some(error),
            Self::Transition(error) => Some(error),
        }
    }
}
