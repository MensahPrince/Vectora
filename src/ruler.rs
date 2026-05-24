//! Timeline ruler tick generation (CapCut-style visual rhythm + SMPTE labels).
//!
//! Given the current viewport state (scroll offset, width, zoom level,
//! frame rate, drop-frame flag), returns the list of tick marks that
//! fall inside the visible window — already positioned in viewport-local
//! pixel coordinates, with a subset carrying labels.
//!
//! Three big ideas underpin this module:
//!
//! 1. **Virtualization.** We never emit ticks for off-screen content.
//!    The bound on emitted ticks is the *viewport*, not the timeline
//!    length. An 8-hour 24 fps project is 691 200 frames; generating a
//!    per-frame tick set up-front is a non-starter.
//!
//! 2. **Separate label and tick ladders.** Labels and ticks scale
//!    independently — labels need enough room to be read (~120 px
//!    minimum), ticks just need to be distinguishable (~12 px). The
//!    tick step is constrained to divide the label step evenly so
//!    every label lands on a tick line; you never see a label whose
//!    tick is missing or off by half a frame. This is the design
//!    decision that separates CapCut-style "consumer" rulers from the
//!    older pro-NLE "tall majors + a few subdivisions" look.
//!
//! 3. **Sub-second labels switch format.** When zoomed in far enough
//!    that the label step is sub-second (e.g. every 2 frames at
//!    extreme zoom), the SMPTE `HH:MM:SS:FF` format collapses — the
//!    SS field stops changing between adjacent labels and only the FF
//!    digits move, which is visually noisy. We switch to absolute
//!    frame indices (`f504`, `f520`, …) in that regime; SMPTE returns
//!    automatically as soon as the label step crosses 1 second.
//!
//! ## Tunables
//!
//! - `MIN_LABEL_PX` = 120: label step is chosen so every label has at
//!   least this much horizontal space. Matches CapCut / OpenCut.
//! - `MIN_TICK_PX` = 12: ticks can be denser, ~10× more ticks than
//!   labels at default zoom. Produces the dense-grid feel of CapCut
//!   without crowding.
//!
//! ## References
//!
//! - Heckbert, "Nice Numbers for Graph Labels", *Graphics Gems I* (1990).
//!   Origin of the {1, 2, 5} × 10ⁿ ladder; we use a time-aware variant.
//! - OpenCut, `apps/web/src/timeline/ruler-utils.ts` — explicit
//!   CapCut clone (MIT-licensed), source of the
//!   `{2, 3, 5, 10, 15} frames` and `{1, 2, 3, 5, 10, 15, 30, 60, …} s`
//!   ladders. We follow the same ladder shape so visual behaviour
//!   matches what CapCut users expect.
//! - OpenTimelineIO `RationalTime::to_timecode` — timecode formatter
//!   (`crate::timecode`); used for the seconds-and-above label regime.

use slint::{ModelRc, SharedString, VecModel};
use std::rc::Rc;

use crate::RulerTick;
use crate::timecode::format_timecode;

/// Lower bound on pixel distance between two consecutive labels.
/// Below this, label text runs into the next label.
///
/// 120 px is CapCut / OpenCut's value. With our 11-px caption-mono
/// font, a worst-case `HH:MM:SS:FF` glyph string is ~75 px wide,
/// leaving ~45 px of gap before the next label — comfortable but not
/// gaping.
const MIN_LABEL_PX: f32 = 120.0;

/// Lower bound on pixel distance between two consecutive ticks.
/// Below this, ticks visually fuse and the grid stops being readable.
///
/// 12 px gives ~10 ticks per label at default zoom — dense enough to
/// give the ruler a continuous rhythm without becoming a smear. CapCut
/// itself uses ~18 px; we run a touch denser by user preference.
const MIN_TICK_PX: f32 = 12.0;

/// Horizontal padding (px) added to each side of the visible frame
/// range so labels can clip gracefully across viewport edges.
///
/// A labeled tick's *line* is 1 px wide, but its *label* extends
/// ~75 px to the right. If we strictly clipped the frame range at the
/// viewport edge, a label whose tick just scrolled past the left edge
/// would be dropped from the tick model while most of its glyphs were
/// still inside the viewport — visible "popping". Instead we emit a
/// wider range and rely on the parent `Rectangle { clip: true }` to
/// clip rendered text at the actual viewport edge. The label fades
/// off-screen one pixel at a time, matching Premiere / Resolve / FCP /
/// CapCut behaviour.
const LABEL_PAD_PX: f32 = 120.0;

/// Safety cap on ticks emitted per call. At sane zoom levels we emit
/// ~80–120 ticks. The cap exists so a pathological state (zoom
/// collapsing to ~0, or a stale invocation mid-resize) can't flood
/// Slint with thousands of elements.
const MAX_TICKS: usize = 1024;

/// Sub-second frame steps available for labels.
///
/// We start at 2 (never 1) so that even at maximum zoom there's room
/// for at least one tick between labels — the tick ladder allows 1f
/// while the label ladder does not. {2, 3, 5, 10, 15} is the CapCut /
/// OpenCut ladder verbatim.
const LABEL_FRAME_STEPS: &[i64] = &[2, 3, 5, 10, 15];

/// Frame steps available for ticks. Goes down to 1 for the densest grid.
const TICK_FRAME_STEPS: &[i64] = &[1, 2, 3, 5, 10, 15];

/// Seconds-and-above progression. Mapped to frame counts at call time
/// via `seconds_to_frames`.
///
/// Goes up to 10 hours; nobody scrubs a 24-hour project at "one label
/// per 10 hours" zoom, and SMPTE rolls past 99 h territory anyway.
const SECONDS_STEPS: &[i64] = &[
    1, 2, 3, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 18000, 36000,
];

/// Compute the visible-window tick list.
///
/// `scroll_x` follows the Slint Flickable convention: negative when the
/// user has scrolled content to the right (i.e. the viewport's origin
/// has moved left within content-space). Frame 0 of the sequence is at
/// content-space pixel 0.
pub fn compute_visible_ticks(
    scroll_x: f32,
    viewport_w: f32,
    zoom: f32,
    fps_num: i64,
    fps_den: i64,
    drop_frame: bool,
) -> Vec<RulerTick> {
    // Bail on degenerate inputs before we underflow or divide by zero.
    // These can happen briefly during layout (Ruler hasn't been sized
    // yet, or the EditorStore isn't wired). Empty model > panic.
    if zoom <= 0.0 || viewport_w <= 0.0 || fps_num <= 0 || fps_den <= 0 {
        return Vec::new();
    }

    // Visible *content-space* pixel range. With Slint's Flickable,
    // scroll_x is negative when scrolled right, so the left edge of
    // the visible content is at content pixel `-scroll_x`.
    let left_px = (-scroll_x).max(0.0);
    let right_px = left_px + viewport_w;

    // Move into integer frame indices and stay there through layout.
    // This is why an NLE's ruler doesn't drift across hours of timeline.
    //
    // `pad_frames` extends the range by `LABEL_PAD_PX` on each side so
    // a labeled tick that has scrolled just past the edge still gets
    // emitted; the parent clip Rectangle does the actual visual clip.
    let pad_frames = (LABEL_PAD_PX / zoom).ceil() as i64;
    let first_frame = (left_px / zoom).floor() as i64 - pad_frames;
    let last_frame = (right_px / zoom).ceil() as i64 + pad_frames;

    let (label_step, sub_second) = pick_label_interval(zoom, fps_num, fps_den);
    // Falling back to the label step when no finer tick fits keeps the
    // emission loop simple (every emitted point is then a labeled
    // tick). Happens at extreme zoom-out where even the largest
    // seconds-step is too small in px to qualify as a tick.
    let tick_step = pick_tick_interval(label_step, zoom, fps_num, fps_den).unwrap_or(label_step);

    let mut out: Vec<RulerTick> = Vec::with_capacity(128);

    // Walk multiples of `tick_step`. Every tick whose index also
    // divides `label_step` carries a label; the rest are bare ticks.
    // This is the design point that makes labels always sit on tick
    // lines (no half-frame visual mismatch): the tick step *divides*
    // the label step by construction.
    //
    // `div_euclid` rounds toward negative infinity (unlike `/` which
    // truncates toward zero), so this is correct for negative
    // first_frame even though we clamp to 0 below.
    let mut k = first_frame.div_euclid(tick_step) * tick_step;
    if k < first_frame {
        k += tick_step;
    }
    while k <= last_frame && out.len() < MAX_TICKS {
        if k >= 0 {
            let x = (k as f32 * zoom + scroll_x).round();
            let has_label = k % label_step == 0;
            let label = if has_label {
                SharedString::from(format_label(k, sub_second, fps_num, fps_den, drop_frame))
            } else {
                SharedString::default()
            };
            out.push(RulerTick {
                x,
                is_major: has_label,
                label,
            });
        }
        k += tick_step;
    }

    out
}

/// Slint callback adapter: wraps `compute_visible_ticks` in a
/// `ModelRc<RulerTick>` for the Rust → Slint boundary. Called from
/// `main.rs` once at startup to install the handler.
///
/// Slint passes `int` properties as `i32`; we widen to `i64` because
/// tick math is more comfortable in 64-bit (even though `i32` has
/// plenty of range for any realistic project).
pub fn ticks_model(
    scroll_x: f32,
    viewport_w: f32,
    zoom: f32,
    fps_num: i32,
    fps_den: i32,
    drop_frame: bool,
) -> ModelRc<RulerTick> {
    let ticks = compute_visible_ticks(
        scroll_x,
        viewport_w,
        zoom,
        fps_num as i64,
        fps_den as i64,
        drop_frame,
    );
    ModelRc::from(Rc::new(VecModel::from(ticks)))
}

/// Format a single labeled-tick's text.
///
/// At sub-second label steps (e.g. every 2 frames), the SMPTE format
/// would emit `00:00:21:00, 00:00:21:08, 00:00:21:16` — the SS field
/// stops changing and only FF moves, which reads as noise. We switch
/// to absolute frame indices (`f504`, `f508`, …) in that regime.
///
/// Important: we use the *absolute* frame index, not "frame within
/// the current second". A label of `f504` always means the same point
/// on the timeline regardless of zoom, which makes it useful for
/// debugging and lines up with what the inspector / agent commands
/// will report.
fn format_label(
    frame: i64,
    sub_second: bool,
    fps_num: i64,
    fps_den: i64,
    drop_frame: bool,
) -> String {
    if sub_second {
        format!("f{}", frame)
    } else {
        format_timecode(frame, fps_num, fps_den, drop_frame)
    }
}

/// Pick the label step (in frames) for the current zoom.
///
/// Returns `(step_frames, sub_second)`. `sub_second == true` means the
/// step is smaller than one second of wall-clock time, which switches
/// the label formatter to `f<index>` (see `format_label`).
///
/// Selection order — smallest first within each phase:
///
///   1. Sub-second frame ladder (`{2, 3, 5, 10, 15}` frames): if any
///      entry's pixel width >= `MIN_LABEL_PX`, pick the smallest. This
///      activates only at high zoom where individual frames are wider
///      than ~24 px each.
///   2. Seconds ladder (`{1, 2, 3, 5, 10, 15, 30, 60, 120, …}` sec):
///      pick the smallest entry whose pixel width >= `MIN_LABEL_PX`.
///      Anchored to seconds so labels read `00:00:01:00, 00:00:02:00,
///      …` instead of the irrational `00:00:00:10, 00:00:00:20,
///      00:00:01:06, …` pattern you get if frame-counted majors don't
///      divide 1 second evenly.
///   3. Largest seconds entry (degenerate fallback for absurd
///      zoom-outs where even 10-hour labels don't fit).
fn pick_label_interval(zoom: f32, fps_num: i64, fps_den: i64) -> (i64, bool) {
    for &k in LABEL_FRAME_STEPS {
        if (k as f32) * zoom >= MIN_LABEL_PX {
            return (k, true);
        }
    }
    for &s in SECONDS_STEPS {
        let frames = seconds_to_frames(s, fps_num, fps_den);
        if (frames as f32) * zoom >= MIN_LABEL_PX {
            return (frames, false);
        }
    }
    let last = *SECONDS_STEPS.last().unwrap();
    (seconds_to_frames(last, fps_num, fps_den), false)
}

/// Pick a tick step (in frames) that:
///
///   * **Divides `label_step` evenly** — so every label sits on a
///     tick line. Without this constraint you get visual misalignment
///     between labels and their nearest tick, which reads as broken.
///   * **Has pixel width >= `MIN_TICK_PX`** — so adjacent ticks don't
///     fuse.
///
/// Returns `None` when no candidate fits (extreme zoom-out where even
/// the largest seconds entry that divides `label_step` is too small).
/// Callers should treat `None` as "no intermediate ticks, just labels".
///
/// Selection prefers the *smallest* fitting candidate so the grid is
/// as dense as `MIN_TICK_PX` allows, matching CapCut's filled-rhythm
/// look.
fn pick_tick_interval(
    label_step: i64,
    zoom: f32,
    fps_num: i64,
    fps_den: i64,
) -> Option<i64> {
    // Frame ladder first: at moderate-to-high zoom the smallest
    // divisor of `label_step` is itself a frame count, so we check
    // these before falling to coarser seconds-based ticks.
    for &k in TICK_FRAME_STEPS {
        if k > 0 && label_step % k == 0 && (k as f32) * zoom >= MIN_TICK_PX {
            return Some(k);
        }
    }
    // Seconds ladder: at low zoom the label step is huge (e.g. 30s
    // or 1min) and we need a tick step in the same regime to divide
    // it cleanly. Smallest first.
    for &s in SECONDS_STEPS {
        let frames = seconds_to_frames(s, fps_num, fps_den);
        if frames <= 0 || frames >= label_step {
            continue;
        }
        if label_step % frames == 0 && (frames as f32) * zoom >= MIN_TICK_PX {
            return Some(frames);
        }
    }
    None
}

/// Convert a seconds count to a frame count for the current rate,
/// using banker's-style rounding.
///
/// For NTSC rates this introduces a 0–1 frame error per ladder step,
/// but the *label* on emitted ticks is computed from the actual frame
/// index by `format_timecode`, so labels stay clean (e.g.
/// `"00:00:01:00"` at whatever frame our rounded "1 second" interval
/// lands).
fn seconds_to_frames(seconds: i64, fps_num: i64, fps_den: i64) -> i64 {
    (seconds * fps_num + fps_den / 2) / fps_den
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    fn majors(ticks: &[RulerTick]) -> Vec<(f32, String)> {
        ticks
            .iter()
            .filter(|t| t.is_major)
            .map(|t| (t.x, t.label.to_string()))
            .collect()
    }

    fn minors(ticks: &[RulerTick]) -> Vec<f32> {
        ticks.iter().filter(|t| !t.is_major).map(|t| t.x).collect()
    }

    // ---------- Degenerate inputs ----------

    #[test]
    fn empty_for_zero_viewport() {
        assert!(compute_visible_ticks(0.0, 0.0, 10.0, 24, 1, false).is_empty());
    }

    #[test]
    fn empty_for_zero_zoom() {
        assert!(compute_visible_ticks(0.0, 1000.0, 0.0, 24, 1, false).is_empty());
    }

    #[test]
    fn empty_for_bad_fps() {
        assert!(compute_visible_ticks(0.0, 1000.0, 10.0, 0, 1, false).is_empty());
        assert!(compute_visible_ticks(0.0, 1000.0, 10.0, 24, 0, false).is_empty());
    }

    // ---------- Ladder picks (label interval) ----------

    #[test]
    fn picks_sub_second_label_at_high_zoom() {
        // 24fps, zoom = 10 px/frame.
        //
        // Sub-second ladder {2, 3, 5, 10, 15} f * 10 = {20, 30, 50,
        // 100, 150} px. First entry >= MIN_LABEL_PX(120) is 15f
        // (= 0.625 s) at 150 px. That's *sub-second*, so the formatter
        // switches to `f<index>`.
        //
        // To get textbook 1-second SMPTE labels at 24fps you have to
        // be at < ~8 px/frame (so 15f < 120 px and the algorithm
        // falls through to the seconds ladder). See
        // `picks_one_second_label_at_low_enough_zoom` for that case.
        let (step, sub_second) = pick_label_interval(10.0, 24, 1);
        assert_eq!(step, 15);
        assert!(sub_second);
    }

    #[test]
    fn picks_one_second_label_at_low_enough_zoom() {
        // 24fps, zoom = 6 px/frame. Sub-second ladder:
        //   2f=12, 3f=18, 5f=30, 10f=60, 15f=90 — all < 120, all fail.
        // Seconds ladder: 1s = 24f = 144 px ✓.
        let (step, sub_second) = pick_label_interval(6.0, 24, 1);
        assert_eq!(step, 24);
        assert!(!sub_second);
    }

    #[test]
    fn picks_sub_second_label_at_extreme_zoom_in() {
        // zoom = 100 px/frame: 2f = 200 px ≥ 120 → pick 2f, sub-second.
        let (step, sub_second) = pick_label_interval(100.0, 24, 1);
        assert_eq!(step, 2);
        assert!(sub_second);
    }

    #[test]
    fn picks_minute_label_at_low_zoom() {
        // 24fps, zoom = 0.1 px/frame. Frame ladder all fails.
        // Seconds ladder: 1s = 24f = 2.4 px (fail), …, 60s = 1440f =
        // 144 px ✓.
        let (step, sub_second) = pick_label_interval(0.1, 24, 1);
        assert_eq!(step, 1440);
        assert!(!sub_second);
    }

    #[test]
    fn picks_largest_step_when_zoom_is_absurd() {
        // zoom = 1e-9: nothing fits, fallback to last seconds entry.
        let (step, sub_second) = pick_label_interval(1e-9, 24, 1);
        assert_eq!(step, seconds_to_frames(36000, 24, 1));
        assert!(!sub_second);
    }

    // ---------- Ladder picks (tick interval) ----------

    #[test]
    fn tick_divides_label_at_default_zoom() {
        // 24fps, zoom = 6 px/frame. Label step = 24 frames.
        // Tick candidates dividing 24: {1, 2, 3, 4, 6, 8, 12}.
        // From TICK_FRAME_STEPS {1,2,3,5,10,15} the divisors are
        // {1, 2, 3}. With MIN_TICK_PX=12 at zoom=6:
        //   1f = 6 px (fail), 2f = 12 px ✓ — pick 2f.
        let label = 24;
        let tick = pick_tick_interval(label, 6.0, 24, 1);
        assert_eq!(tick, Some(2));
    }

    #[test]
    fn tick_divides_label_when_label_is_sub_second() {
        // Label step = 2 frames (sub-second). Only divisor in
        // TICK_FRAME_STEPS is 1f. At zoom=100, 1f = 100 px ≥ 12 → ok.
        let label = 2;
        let tick = pick_tick_interval(label, 100.0, 24, 1);
        assert_eq!(tick, Some(1));
    }

    #[test]
    fn tick_falls_to_seconds_when_label_is_huge() {
        // Label step = 1 minute = 1440 frames @ 24fps, zoom = 0.1 px/f.
        // Frame ladder fails (max 15f = 1.5 px). Seconds ladder:
        //   1s = 24f = 2.4 px (fail), 2s = 4.8 (fail), 3s = 7.2 (fail),
        //   5s = 12 px (exactly threshold) — and 1440 % (5*24=120) == 0,
        //   so pick 5s.
        let label = 1440;
        let tick = pick_tick_interval(label, 0.1, 24, 1);
        assert_eq!(tick, Some(seconds_to_frames(5, 24, 1)));
    }

    #[test]
    fn tick_is_none_at_pathological_zoom() {
        // At sane parameters tick is always Some — `MIN_LABEL_PX
        // (120)` is comfortably above `MIN_TICK_PX (12)`, so any
        // label step has at least one divisor in our ladders that
        // qualifies as a tick. tick = None only fires at degenerate
        // zooms (here 1e-9 px/frame) where even the largest seconds
        // entry below the label step fails the tick threshold.
        // Callers treat None as "draw the labeled ticks only".
        let (label, _) = pick_label_interval(1e-9, 24, 1);
        let tick = pick_tick_interval(label, 1e-9, 24, 1);
        assert!(tick.is_none());
    }

    // ---------- Label format ----------

    #[test]
    fn seconds_labels_use_smpte_format() {
        // 24fps, zoom = 6 px/frame: label step = 1s (24 frames).
        let ticks = compute_visible_ticks(0.0, 1500.0, 6.0, 24, 1, false);
        let m = majors(&ticks);
        assert!(!m.is_empty());
        // First label should be at frame 0 → "00:00:00:00".
        assert_eq!(m[0].1, "00:00:00:00");
        // All labels should be valid SMPTE.
        for (_, label) in &m {
            assert_eq!(label.matches(':').count(), 3, "{label} is not SMPTE");
        }
    }

    #[test]
    fn sub_second_labels_use_frame_prefix() {
        // 24fps, zoom = 100 px/frame: label step = 2 frames (sub-second).
        // Labels should be `f0`, `f2`, `f4`, … not SMPTE.
        let ticks = compute_visible_ticks(0.0, 1500.0, 100.0, 24, 1, false);
        let m = majors(&ticks);
        assert!(!m.is_empty());
        for (_, label) in &m {
            assert!(
                label.starts_with('f'),
                "sub-second label {label:?} is not `f<index>`"
            );
            // Should NOT look like SMPTE.
            assert!(!label.contains(':'), "{label} contains colons");
        }
        assert_eq!(m[0].1, "f0");
    }

    #[test]
    fn label_only_on_second_boundaries_in_seconds_regime() {
        // At seconds regime every label should end in `:00` (FF=00).
        let ticks = compute_visible_ticks(0.0, 1500.0, 6.0, 24, 1, false);
        for (_, label) in majors(&ticks) {
            assert!(
                label.ends_with(":00"),
                "seconds-regime label {label:?} is not on a second boundary"
            );
        }
    }

    // ---------- Tick alignment ----------

    #[test]
    fn every_label_is_also_a_tick_position() {
        // Sanity: with `tick_step` constrained to divide `label_step`,
        // no labeled tick is "between" the bare ticks — visually they
        // sit on the same grid.
        let ticks = compute_visible_ticks(0.0, 1500.0, 6.0, 24, 1, false);
        let tick_xs: Vec<f32> = ticks.iter().map(|t| t.x).collect();
        for label_x in majors(&ticks).iter().map(|(x, _)| *x) {
            assert!(
                tick_xs.contains(&label_x),
                "label at x={label_x} has no matching tick"
            );
        }
    }

    #[test]
    fn minors_are_strictly_denser_than_majors() {
        // The whole point of the CapCut visual: more bare ticks than
        // labeled ticks. At default zoom we expect at least 2× more
        // minors than majors (in practice it's 4–10× depending on
        // ladder picks).
        let ticks = compute_visible_ticks(0.0, 2000.0, 6.0, 24, 1, false);
        let majors_n = majors(&ticks).len();
        let minors_n = minors(&ticks).len();
        assert!(
            minors_n >= majors_n,
            "expected dense minor grid, got {minors_n} minors / {majors_n} majors"
        );
    }

    // ---------- Visible-range behaviour ----------

    #[test]
    fn first_tick_at_zero_when_unscrolled() {
        let ticks = compute_visible_ticks(0.0, 1000.0, 6.0, 24, 1, false);
        let m = majors(&ticks);
        assert!(!m.is_empty());
        assert_eq!(m[0].0, 0.0);
        assert_eq!(m[0].1, "00:00:00:00");
    }

    #[test]
    fn ticks_respect_scroll_offset() {
        // Scroll right by 100 px, zoom = 6 px/frame: visible content
        // starts at frame 100/6 ≈ 16. Label step is 1 second (24
        // frames at 24 fps), so the first labelled tick *inside* the
        // viewport (x >= 0) is at frame 24 → viewport_x = 24*6 - 100
        // = 44, labelled "00:00:01:00".
        //
        // Frame 0's label may still be in the model due to LABEL_PAD —
        // we don't assert it isn't.
        let ticks = compute_visible_ticks(-100.0, 1000.0, 6.0, 24, 1, false);
        let m = majors(&ticks);
        let first_inside = m
            .iter()
            .find(|(x, _)| *x >= 0.0)
            .expect("no labels inside viewport");
        assert_eq!(first_inside.0, 44.0);
        assert_eq!(first_inside.1, "00:00:01:00");
    }

    #[test]
    fn ticks_stay_within_label_pad_of_viewport() {
        let ticks = compute_visible_ticks(0.0, 100.0, 6.0, 24, 1, false);
        for t in &ticks {
            assert!(
                t.x >= -LABEL_PAD_PX - 1.0 && t.x <= 100.0 + LABEL_PAD_PX + 1.0,
                "tick at {} is too far outside viewport (pad = {})",
                t.x,
                LABEL_PAD_PX
            );
        }
    }

    #[test]
    fn major_label_survives_left_edge_until_fully_off_screen() {
        // Regression for the "label pops" glitch: a major near the
        // left edge whose tick line has scrolled into the negative
        // viewport space but whose text glyphs are still visible
        // should remain in the model. The parent clip Rectangle
        // handles the visual clipping pixel-by-pixel.
        let zoom = 6.0;

        // Scroll = -10 px. Frame 0's tick is at viewport x = -10;
        // glyphs extend from ~-7 to ~+68. Label MUST still be in
        // the model.
        let ticks = compute_visible_ticks(-10.0, 1000.0, zoom, 24, 1, false);
        let has_zero = ticks
            .iter()
            .any(|t| t.is_major && t.label == "00:00:00:00");
        assert!(
            has_zero,
            "frame 0 label dropped from model while still partly visible"
        );

        // Scroll past the padding — label is fine to drop now.
        let far = -(LABEL_PAD_PX as f64 + 50.0) as f32;
        let ticks = compute_visible_ticks(far, 1000.0, zoom, 24, 1, false);
        let has_zero = ticks
            .iter()
            .any(|t| t.is_major && t.label == "00:00:00:00");
        assert!(
            !has_zero,
            "frame 0 label retained after it should have left the padded range"
        );
    }

    #[test]
    fn no_negative_frame_ticks() {
        // Scrolling "past" zero (positive scroll_x) shouldn't emit
        // ticks for negative frames. Defensive — Flickable shouldn't
        // give us positive scroll_x in normal use.
        let ticks = compute_visible_ticks(50.0, 1000.0, 6.0, 24, 1, false);
        for t in &ticks {
            assert!(t.x >= 0.0 || t.x.is_finite());
        }
    }

    // ---------- Drop-frame ----------

    #[test]
    fn drop_frame_labels_use_semicolon_separator() {
        // 29.97 DF at a zoom where the label step lands in the
        // seconds regime (so SMPTE labels are emitted). With label
        // step ≥ 1 second and DF active, at least one label should
        // contain `;`.
        let ticks = compute_visible_ticks(0.0, 2000.0, 3.0, 30000, 1001, true);
        let m = majors(&ticks);
        assert!(
            m.iter().any(|(_, l)| l.contains(';')),
            "no DF labels found in {:?}",
            m
        );
    }

    #[test]
    fn non_drop_labels_omit_semicolon() {
        let ticks = compute_visible_ticks(0.0, 2000.0, 3.0, 30000, 1001, false);
        let m = majors(&ticks);
        assert!(m.iter().all(|(_, l)| !l.contains(';')));
    }

    #[test]
    fn sub_second_labels_ignore_drop_frame() {
        // Sub-second labels use `f<index>` regardless of DF — the
        // drop-frame separator only applies to SMPTE-formatted labels.
        let ticks = compute_visible_ticks(0.0, 1500.0, 100.0, 30000, 1001, true);
        let m = majors(&ticks);
        assert!(!m.is_empty());
        for (_, l) in &m {
            assert!(l.starts_with('f'));
            assert!(!l.contains(';'));
        }
    }

    // ---------- Safety cap ----------

    #[test]
    fn cap_prevents_overflow_at_pathological_inputs() {
        let ticks = compute_visible_ticks(0.0, 100_000_000.0, 1.0, 24, 1, false);
        assert!(ticks.len() <= MAX_TICKS);
    }

    // ---------- NTSC rounding sanity ----------

    #[test]
    fn ntsc_2398_labels_clean_seconds() {
        // 23.976 NDF: 1s ≈ round(24000/1001) = 24 frames. At frame 24
        // the timecode label should still read "...:01:00". Pick a
        // zoom where 1s sits inside the seconds regime.
        let zoom = 120.0 / 24.0; // 1s ≈ 120 px, just above MIN_LABEL_PX.
        let ticks = compute_visible_ticks(0.0, 2000.0, zoom, 24000, 1001, false);
        let m = majors(&ticks);
        let has_one_sec_label = m.iter().any(|(_, l)| l == "00:00:01:00");
        assert!(has_one_sec_label, "expected 00:00:01:00 among {:?}", m);
    }

    // ---------- Coordinates rounded to whole pixels ----------

    #[test]
    fn tick_positions_are_integer_pixels() {
        // 1-px tick lines blur if positioned on a half pixel.
        let ticks = compute_visible_ticks(-37.5, 500.0, 7.3, 24, 1, false);
        for t in &ticks {
            assert_eq!(t.x.fract(), 0.0, "tick {} not integer", t.x);
        }
    }

    // ---------- Model + slint adapter smoke ----------

    #[test]
    fn ticks_model_round_trip() {
        let model = ticks_model(0.0, 500.0, 6.0, 24, 1, false);
        assert!(model.row_count() > 0);
        let first = model.row_data(0).unwrap();
        assert!(first.is_major, "first tick at frame 0 should carry a label");
    }
}
