use super::*;

/// A preview render slower than this logs a `warn` (default-visible): at
/// this point the frame is why the app feels stuck after an edit, so leave
/// a breadcrumb attributing it (the renderer's stage log tells the rest).
pub(super) const SLOW_PREVIEW_WARN_MS: f64 = 250.0;

/// Byte cap for [`FrameCache`]: at a typical fit size (~1280×720 RGBA,
/// ~3.7 MB) this holds ~70 recently displayed frames — several seconds of
/// scrub-back headroom — while staying far below what decode already uses.
const FRAME_CACHE_BYTES: usize = 256 << 20;

/// Largest per-request tick step still treated as steady forward motion
/// worth speculating one step past (1 = real-time playback and stepping;
/// larger = fast-forward playback or a brisk forward drag). Beyond this the
/// pattern is a jump, and predicting the next target is a coin flip.
pub(super) const MAX_SPECULATIVE_STEP: i64 = 8;

/// What a composited preview frame was rendered *from*: any difference in
/// these means the pixels may differ.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct FrameKey {
    /// Timeline tick (the playhead).
    pub(super) tick: i64,
    /// [`Engine::revision`] — bumped by every project mutation, so an edit,
    /// undo, or session swap can never serve pre-edit pixels.
    pub(super) revision: u64,
    /// The fit bound the render was requested at (`None` = full canvas).
    /// Captures the bucketed viewport, quality tier, and long-side cap in
    /// one value — exactly what changes the output resolution.
    pub(super) bound: Option<(u32, u32)>,
}

pub(super) struct CacheEntry {
    buffer: SharedPixelBuffer<Rgba8Pixel>,
    /// Logical timestamp of the last hit/insert (LRU order).
    used: u64,
}

/// LRU cache of delivered preview frames. Holding the [`SharedPixelBuffer`]
/// itself is cheap: it's refcounted, and the UI holds a clone of the same
/// allocation anyway. Hits make hover jitter and re-visited scrub ground
/// free — the only realistic fix for backward scrubbing over long-GOP
/// sources short of proxy media (100–320 ms per re-composite today).
///
/// Interior mutability so the worker loop and its `mutate` closure can share
/// it like [`FrameFit`]; single-threaded access (worker thread only).
#[derive(Default)]
pub(super) struct FrameCache {
    pub(super) entries: RefCell<HashMap<FrameKey, CacheEntry>>,
    pub(super) bytes: Cell<usize>,
    pub(super) clock: Cell<u64>,
}

impl FrameCache {
    pub(super) fn stats(&self) -> PreviewCacheStats {
        PreviewCacheStats {
            entries: self.entries.borrow().len(),
            // Every supported Rust target's `usize` fits in `u64`; the cast is
            // exact and gives the cache registry a platform-stable unit.
            bytes: self.bytes.get() as u64,
        }
    }

    /// Drop every delivered frame — for state changes the revision key
    /// doesn't cover (a proxy binding swaps decode sources without touching
    /// the project). Returns the exact usage that was removed.
    pub(super) fn clear(&self) -> PreviewCacheStats {
        let removed = self.stats();
        self.entries.borrow_mut().clear();
        self.bytes.set(0);
        self.clock.set(0);
        removed
    }

    pub(super) fn get(&self, key: &FrameKey) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        let mut entries = self.entries.borrow_mut();
        let entry = entries.get_mut(key)?;
        self.clock.set(self.clock.get() + 1);
        entry.used = self.clock.get();
        Some(entry.buffer.clone())
    }

    pub(super) fn insert(&self, key: FrameKey, buffer: SharedPixelBuffer<Rgba8Pixel>) {
        let mut entries = self.entries.borrow_mut();
        self.clock.set(self.clock.get() + 1);
        let bytes = buffer.as_bytes().len();
        if let Some(old) = entries.insert(
            key,
            CacheEntry {
                buffer,
                used: self.clock.get(),
            },
        ) {
            self.bytes
                .set(self.bytes.get() - old.buffer.as_bytes().len());
        }
        self.bytes.set(self.bytes.get() + bytes);

        // Evict least-recently-used until under budget. The scan is O(n) but
        // runs only on inserts that overflow, over at most a few hundred
        // entries — noise next to the composite that preceded it.
        while self.bytes.get() > FRAME_CACHE_BYTES && entries.len() > 1 {
            let Some(oldest) = entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| *k) else {
                break;
            };
            if let Some(evicted) = entries.remove(&oldest) {
                self.bytes
                    .set(self.bytes.get() - evicted.buffer.as_bytes().len());
            }
        }
    }
}
