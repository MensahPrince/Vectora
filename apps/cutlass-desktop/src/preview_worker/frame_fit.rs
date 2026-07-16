use super::*;

/// The preview fit bound + quality ladder (the Swift `PreviewFeed` model,
/// worker-side): renders fit inside the reported viewport, and a slow run of
/// renders steps the resolution down a tier so scrubbing stays interactive.
///
/// The ladder is fed the **resolution-dependent** share of each render
/// (raster + composite + readback, [`cutlass_engine::FrameStats`]), never the
/// decode time: decode runs at the source's native size whatever the output
/// bound is, so dropping tiers can't buy decode time back — it would only
/// blur the preview while staying just as slow (the field failure mode this
/// replaces: 4K long-GOP seeks flooring the tier over and over).
/// `Cell`s because the coalescing loop and the `mutate` closure both repaint.
pub(super) struct FrameFit {
    /// Bucketed viewport (physical px); `(0, 0)` until the panel reports.
    viewport: Cell<(u32, u32)>,
    /// EMA of recent scaled (resolution-dependent) render costs, milliseconds.
    avg_ms: Cell<f64>,
    /// Index into [`QUALITY_LADDER`].
    pub(super) tier: Cell<usize>,
    /// Evidence window backing tier *raises* (reset on every tier change and
    /// every [`RAISE_WINDOW_SAMPLES`] renders): how many renders it spans,
    /// the worst cost seen, and when the tier was last changed. Field logs
    /// showed why raises must demand more than a fast EMA: sequential-decode
    /// frames render in ~15ms even while seeks cost whole seconds, so one
    /// cheap frame right after a drop re-raised the tier and the next seek
    /// slammed it back down — a resolution pop plus a wasted slow frame per
    /// cycle.
    samples: Cell<u32>,
    max_ms: Cell<f64>,
    pub(super) changed_at: Cell<Instant>,
}

/// Resolution multipliers, best first. Tier moves down when renders run slow
/// and back up when they're comfortably fast. The 0.35 floor exists for weak
/// iGPUs (128MB-class shared-memory parts): decode cost is resolution-fixed,
/// but composite + readback scale with output pixels, and at the floor
/// they're a rounding error next to the decode.
pub(super) const QUALITY_LADDER: [f64; 4] = [1.0, 0.7, 0.5, 0.35];
/// Never request more than this many pixels on the long side — retina
/// monitors ask for huge viewports whose cost buys invisible detail.
const MAX_LONG_SIDE: f64 = 1440.0;
/// Fit bound used until the panel reports its size: full HD keeps the launch
/// frame cheap on 4K-canvas projects (a full-canvas composite + readback of
/// 3840×2160 was the first-frame stall) while staying sharper than any
/// realistic panel needs before layout settles.
pub(super) const UNREPORTED_VIEWPORT_BOUND: (u32, u32) = (1280, 720);
/// Scaled-cost EMA above this drops a tier; below `RAISE_BELOW_MS` climbs
/// back. Thresholds gate on the resolution-dependent share of the render
/// (raster + composite + readback) — see [`FrameFit`].
const DROP_ABOVE_MS: f64 = 45.0;
const RAISE_BELOW_MS: f64 = 18.0;
/// A single render whose *scaled* cost is this far past budget skips the EMA
/// and drops straight to the bottom tier: walking down one tier per render
/// costs one multi-second frame per step on the machines that need the floor
/// most.
const HARD_DROP_MS: f64 = 360.0;
/// Raising a tier requires all of: this long at the current tier, at least
/// [`RAISE_MIN_SAMPLES`] renders folded in, and no single render in the
/// evidence window over [`DROP_ABOVE_MS`]. Drops stay immediate — the
/// asymmetry trades a possibly-conservative resolution for never popping.
pub(super) const RAISE_MIN_DWELL: Duration = Duration::from_secs(3);
pub(super) const RAISE_MIN_SAMPLES: u32 = 8;
/// The evidence window turns over after this many renders, so one old spike
/// can't veto raises forever (e.g. after an export or a burst of seeks).
pub(super) const RAISE_WINDOW_SAMPLES: u32 = 32;
/// Viewport reports quantize to this grid so a live window resize (a report
/// per frame) doesn't churn re-renders for sub-bucket changes.
const VIEWPORT_BUCKET: u32 = 64;

impl Default for FrameFit {
    fn default() -> Self {
        Self {
            viewport: Cell::new((0, 0)),
            avg_ms: Cell::new(0.0),
            tier: Cell::new(0),
            samples: Cell::new(0),
            max_ms: Cell::new(0.0),
            changed_at: Cell::new(Instant::now()),
        }
    }
}

impl FrameFit {
    /// Record a new on-screen size. Returns whether the bucketed bound
    /// changed (⇒ the caller should repaint the current frame).
    pub(super) fn set_viewport(&self, width: u32, height: u32) -> bool {
        let bucket = |v: u32| v.max(1).div_ceil(VIEWPORT_BUCKET) * VIEWPORT_BUCKET;
        let next = (bucket(width), bucket(height));
        let changed = self.viewport.get() != next;
        self.viewport.set(next);
        changed
    }

    /// The `(max_width, max_height)` to request for the next render: the
    /// viewport scaled by the current quality tier, capped at
    /// [`MAX_LONG_SIDE`]. Before the panel reports its size, a conservative
    /// [`UNREPORTED_VIEWPORT_BOUND`] — never the full canvas, which on a 4K
    /// project made the launch frame composite + read back 8.3 MP for pixels
    /// the panel can't show.
    pub(super) fn fit_bound(&self) -> Option<(u32, u32)> {
        let (w, h) = self.viewport.get();
        if w == 0 || h == 0 {
            return Some(UNREPORTED_VIEWPORT_BOUND);
        }
        let mut scale = QUALITY_LADDER[self.tier.get()];
        let long_side = f64::from(w.max(h)) * scale;
        if long_side > MAX_LONG_SIDE {
            scale *= MAX_LONG_SIDE / long_side;
        }
        let dim = |v: u32| ((f64::from(v) * scale).round() as u32).max(1);
        Some((dim(w), dim(h)))
    }

    /// Fold one render's **scaled cost** — the resolution-dependent share
    /// (raster + composite + readback), *not* the whole-frame time — into the
    /// EMA and step the quality tier: slow renders drop a tier immediately
    /// (their cost dominates the average), a catastrophically slow one jumps
    /// straight to the floor, and only *sustained, uniformly fast* evidence
    /// climbs back up (dwell + sample floor + no spike in the window — see
    /// the struct field docs for the thrash this prevents). Decode time is
    /// deliberately excluded: it doesn't respond to resolution, so folding it
    /// in floored the tier on every long-GOP seek without making anything
    /// faster. Tier changes log at `info` — they are rare, and on slow
    /// machines they're the trace of the ladder doing its job.
    pub(super) fn note_render_cost(&self, scaled_ms: f64) {
        // Fresh evidence window past the turnover point (tier unchanged).
        if self.samples.get() >= RAISE_WINDOW_SAMPLES {
            self.samples.set(0);
            self.max_ms.set(0.0);
        }

        let ms = scaled_ms;
        let avg = self.avg_ms.get();
        let next = if avg == 0.0 {
            ms
        } else {
            avg * 0.75 + ms * 0.25
        };
        self.avg_ms.set(next);
        self.samples.set(self.samples.get() + 1);
        self.max_ms.set(self.max_ms.get().max(ms));

        let tier = self.tier.get();
        let bottom = QUALITY_LADDER.len() - 1;
        if ms > HARD_DROP_MS && tier < bottom {
            self.change_tier(bottom);
            info!(
                tier = bottom,
                scaled_ms = %format_args!("{ms:.0}"),
                "preview quality floored by a very slow composite"
            );
        } else if next > DROP_ABOVE_MS && tier + 1 < QUALITY_LADDER.len() {
            self.change_tier(tier + 1);
            info!(
                tier = tier + 1,
                avg_ms = %format_args!("{next:.0}"),
                "preview quality tier down"
            );
        } else if next < RAISE_BELOW_MS
            && tier > 0
            && self.samples.get() >= RAISE_MIN_SAMPLES
            && self.max_ms.get() < DROP_ABOVE_MS
            && self.changed_at.get().elapsed() >= RAISE_MIN_DWELL
        {
            self.change_tier(tier - 1);
            info!(
                tier = tier - 1,
                avg_ms = %format_args!("{next:.0}"),
                "preview quality tier up"
            );
        }
    }

    /// Move to `tier` and restart the cost average and the raise-evidence
    /// window: costs at the old resolution say nothing about the new one.
    fn change_tier(&self, tier: usize) {
        self.tier.set(tier);
        self.avg_ms.set(0.0);
        self.samples.set(0);
        self.max_ms.set(0.0);
        self.changed_at.set(Instant::now());
    }
}
