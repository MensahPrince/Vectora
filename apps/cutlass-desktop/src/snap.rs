//! Drag placement helpers operating on the Slint view model.
//!
//! While the user drags (a clip, or a library tile), the gesture layer calls
//! these every frame; the same resolution is used for the live ghost and for
//! the commit on release, so the preview can never disagree with the drop.
//!
//! Policy follows CapCut:
//! - magnet snapping pulls the dragged edges to clip edges on *all* lanes,
//!   the playhead, and tick 0, with a vertical guide line at the snap point;
//! - lanes only accept their own kind; hovering a foreign-kind lane, empty
//!   space, or a conflicting (overlapping) span resolves to a *new lane*
//!   inserted at the hovered row;
//! - with the main-track magnet on, the bottom video lane stays gapless:
//!   hovering it resolves to an *insertion* between clips (caret instead of
//!   a free landing ghost); the commit shifts later clips right.

use slint::{Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;

use crate::{
    ClipDragResolution, ClipTrimResolution, GroupGhost, LibraryDropResolution, Sequence,
    SnapResult, TrackKind, TransitionJunctionResolution,
};

pub fn compute_drag_snap(
    sequence: &Sequence,
    dragging_source_track_id: &str,
    dragging_clip_id: &str,
    cursor_start_value: i32,
    clip_duration_ticks: i32,
    snap_threshold_ticks: i32,
    playhead_tick: i32,
) -> SnapResult {
    if snap_threshold_ticks <= 0 {
        return SnapResult::none(cursor_start_value);
    }

    let cursor_end = cursor_start_value.saturating_add(clip_duration_ticks);
    let mut best: Option<(i32, i32, i32)> = None;

    // A clip shorter than two thresholds has overlapping edge zones: both
    // edges pinning means a candidate (say the playhead) holds the clip
    // continuously across a span wider than the clip itself, with no free
    // placement anywhere near it — a tiny clip feels stuck. Snap such clips
    // by their leading edge only. (Duration 0 — the trim resolvers'
    // single-point snap — is unaffected: both edges coincide.)
    let consider_trailing = clip_duration_ticks >= 2 * snap_threshold_ticks;

    let mut consider = |candidate: i32| {
        let d_leading = (candidate - cursor_start_value).abs();
        if d_leading <= snap_threshold_ticks && best.is_none_or(|(d, _, _)| d_leading < d) {
            best = Some((d_leading, candidate, candidate));
        }
        if !consider_trailing {
            return;
        }
        let d_trailing = (candidate - cursor_end).abs();
        if d_trailing <= snap_threshold_ticks && best.is_none_or(|(d, _, _)| d_trailing < d) {
            let snapped_start = candidate.saturating_sub(clip_duration_ticks);
            best = Some((d_trailing, snapped_start, candidate));
        }
    };

    consider(0);
    consider(playhead_tick);

    for marker_idx in 0..sequence.markers.row_count() {
        if let Some(marker) = sequence.markers.row_data(marker_idx) {
            consider(marker.tick);
        }
    }

    for track_idx in 0..sequence.tracks.row_count() {
        let Some(track) = sequence.tracks.row_data(track_idx) else {
            continue;
        };
        for clip_idx in 0..track.clips.row_count() {
            let Some(clip) = track.clips.row_data(clip_idx) else {
                continue;
            };
            if track.id == dragging_source_track_id && clip.id == dragging_clip_id {
                continue;
            }
            let s = clip.timeline_start.value;
            let e = s.saturating_add(clip.source_range.duration.value);
            consider(s);
            consider(e);
            // Beat markers (M8 Phase 6): the magnet pulls clip edges onto the
            // music's beats, already published as absolute sequence ticks.
            for beat_idx in 0..clip.beats.row_count() {
                if let Some(beat) = clip.beats.row_data(beat_idx) {
                    consider(beat);
                }
            }
        }
    }

    match best {
        None => SnapResult::none(cursor_start_value),
        Some((_, snapped_start, line)) => SnapResult {
            has_snap: true,
            snapped_start_value: snapped_start,
            snap_line_tick: line,
        },
    }
}

/// Resolve where a clip being dragged by (`dx_ticks`, `hover_row`) would land.
///
/// `hover_row` is the lane-list row under the dragged clip's vertical center
/// (top-first; may be out of range, meaning above/below the existing lanes).
/// With `main_magnet` on, hovering the main lane (bottom video lane) with a
/// video clip resolves to an insertion between clips instead of a free spot.
/// O(total clips) per call — evaluated per drag frame, fine at editing scale.
#[allow(clippy::too_many_arguments)]
pub fn resolve_clip_drag(
    sequence: &Sequence,
    source_track_id: &str,
    dragging_clip_id: &str,
    dx_ticks: i32,
    hover_row: i32,
    playhead_tick: i32,
    snap_threshold_ticks: i32,
    main_magnet: bool,
) -> ClipDragResolution {
    let track_count = sequence.tracks.row_count() as i32;
    let Some((source_kind, orig_start, duration)) =
        find_dragged_clip(sequence, source_track_id, dragging_clip_id)
    else {
        return ClipDragResolution::invalid();
    };

    let desired = (orig_start.saturating_add(dx_ticks)).max(0);

    // Main-track magnet: the bottom video lane is gapless, so a video clip
    // hovering it lands *between* clips (the commit shifts later clips right
    // to open the hole). The edge magnet is irrelevant here — position is
    // quantized to clip boundaries, picked by the unsnapped left edge.
    if main_magnet
        && source_kind == TrackKind::Video
        && main_video_row(sequence) == Some(hover_row)
        && let Some(track) = sequence.tracks.row_data(hover_row as usize)
        && !track.locked
    {
        let exclude = (track.id == source_track_id).then_some(dragging_clip_id);
        let ins = resolve_insertion(&track, exclude, desired);
        return ClipDragResolution {
            valid: true,
            is_new_lane: false,
            target_track_id: track.id.clone(),
            target_row: hover_row,
            resolved_start: ins.commit_tick,
            duration_ticks: duration,
            has_snap: false,
            snap_line_tick: 0,
            is_noop: ins.noop,
            is_insert: true,
            caret_tick: ins.display_tick,
        };
    }

    let snap = compute_drag_snap(
        sequence,
        source_track_id,
        dragging_clip_id,
        desired,
        duration,
        snap_threshold_ticks,
        playhead_tick,
    );
    let snapped = snap.snapped_start_value.max(0);

    // Hovered lane accepts the clip only when kinds match and it isn't locked.
    let hover_track = (0..track_count)
        .contains(&hover_row)
        .then(|| sequence.tracks.row_data(hover_row as usize))
        .flatten()
        .filter(|t| t.kind == source_kind && !t.locked);

    if let Some(track) = hover_track {
        let exclude = (track.id == source_track_id).then_some(dragging_clip_id);
        if span_is_free(&track, snapped, duration, exclude) {
            return ClipDragResolution {
                valid: true,
                is_new_lane: false,
                target_track_id: track.id.clone(),
                target_row: hover_row,
                resolved_start: snapped,
                duration_ticks: duration,
                has_snap: snap.has_snap,
                snap_line_tick: snap.snap_line_tick,
                is_noop: track.id == source_track_id && snapped == orig_start,
                is_insert: false,
                caret_tick: 0,
            };
        }
        // The snap pulled us into a conflict the raw position doesn't have —
        // prefer landing free without the magnet over forcing a new lane.
        if snap.has_snap && snapped != desired && span_is_free(&track, desired, duration, exclude) {
            return ClipDragResolution {
                valid: true,
                is_new_lane: false,
                target_track_id: track.id.clone(),
                target_row: hover_row,
                resolved_start: desired,
                duration_ticks: duration,
                has_snap: false,
                snap_line_tick: 0,
                is_noop: track.id == source_track_id && desired == orig_start,
                is_insert: false,
                caret_tick: 0,
            };
        }
    }

    // Foreign kind, out of range, or conflicting span: a new lane is inserted
    // at the hovered row (clamped to just above / just below the stack).
    ClipDragResolution {
        valid: true,
        is_new_lane: true,
        target_track_id: Default::default(),
        target_row: hover_row.clamp(0, track_count),
        resolved_start: snapped,
        duration_ticks: duration,
        has_snap: snap.has_snap,
        snap_line_tick: snap.snap_line_tick,
        is_noop: false,
        is_insert: false,
        caret_tick: 0,
    }
}

/// Resolve where a library tile dropped at (`cursor_tick`, `drop_row`) lands.
///
/// Freeform: a `lane_kind` lane under the cursor (or empty ⇒ the worker
/// creates one at `drop_row`), position magneted to clip edges / playhead /
/// tick 0. Media tiles target video lanes; generated tiles (text titles,
/// solids, shapes) target their own kind. With `main_magnet` on, dropping a
/// *video* tile on the main lane resolves to an insertion between clips (the
/// worker ripple-inserts, shifting later clips right); generated lanes are
/// never the main track, so they always land freeform.
#[allow(clippy::too_many_arguments)]
pub fn resolve_library_drop(
    sequence: &Sequence,
    lane_kind: TrackKind,
    duration_ticks: i32,
    cursor_tick: i32,
    drop_row: i32,
    playhead_tick: i32,
    snap_threshold_ticks: i32,
    main_magnet: bool,
) -> LibraryDropResolution {
    let track_count = sequence.tracks.row_count() as i32;
    let mut drop_row = drop_row;
    let mut row_track = (0..track_count)
        .contains(&drop_row)
        .then(|| sequence.tracks.row_data(drop_row as usize))
        .flatten()
        .filter(|t| t.kind == lane_kind && !t.locked);

    // A video drop that misses every video lane falls back to the *empty*
    // main track (CapCut: the first video dragged anywhere fills the main
    // lane) — mirrors the worker's landing so the ghost never lies.
    if lane_kind == TrackKind::Video
        && row_track.is_none()
        && let Some(main_row) = main_video_row(sequence)
        && let Some(main) = sequence.tracks.row_data(main_row as usize)
        && main.clips.row_count() == 0
        && !main.locked
    {
        drop_row = main_row;
        row_track = Some(main);
    }

    if lane_kind == TrackKind::Video
        && main_magnet
        && main_video_row(sequence) == Some(drop_row)
        && let Some(track) = &row_track
    {
        let ins = resolve_insertion(track, None, cursor_tick.max(0));
        return LibraryDropResolution {
            target_track_id: track.id.clone(),
            target_row: drop_row,
            resolved_start: ins.commit_tick,
            is_insert: true,
            caret_tick: ins.display_tick,
            has_snap: false,
            snap_line_tick: 0,
        };
    }

    // Magnet against clip edges, the playhead, and tick 0. Empty drag ids ⇒
    // every placed clip is a snap candidate.
    let snap = compute_drag_snap(
        sequence,
        "",
        "",
        cursor_tick,
        duration_ticks,
        snap_threshold_ticks,
        playhead_tick,
    );
    // A snap pulled below tick 0 is clamped away (no guide there).
    let resolved_start = snap.snapped_start_value.max(0);
    let has_snap = snap.has_snap && resolved_start == snap.snapped_start_value;
    LibraryDropResolution {
        target_track_id: row_track.map(|t| t.id.clone()).unwrap_or_default(),
        target_row: drop_row,
        resolved_start,
        is_insert: false,
        caret_tick: 0,
        has_snap,
        snap_line_tick: if has_snap { snap.snap_line_tick } else { 0 },
    }
}

/// Find the nearest abutting clip junction on `hover_row` within
/// `snap_threshold_ticks` of `cursor_tick` (transition drag-and-drop).
pub fn resolve_transition_junction(
    sequence: &Sequence,
    cursor_tick: i32,
    hover_row: i32,
    snap_threshold_ticks: i32,
) -> TransitionJunctionResolution {
    if snap_threshold_ticks <= 0 {
        return TransitionJunctionResolution {
            valid: false,
            left_clip_id: Default::default(),
            cut_tick: 0,
            row: hover_row,
        };
    }
    let track_count = sequence.tracks.row_count() as i32;
    if hover_row < 0 || hover_row >= track_count {
        return TransitionJunctionResolution {
            valid: false,
            left_clip_id: Default::default(),
            cut_tick: 0,
            row: hover_row,
        };
    }
    let Some(track) = sequence.tracks.row_data(hover_row as usize) else {
        return TransitionJunctionResolution {
            valid: false,
            left_clip_id: Default::default(),
            cut_tick: 0,
            row: hover_row,
        };
    };
    if track.locked {
        return TransitionJunctionResolution {
            valid: false,
            left_clip_id: Default::default(),
            cut_tick: 0,
            row: hover_row,
        };
    }
    // Transitions need lanes whose clips composite as frames: audio has no
    // frame, and effect/filter/adjustment segments resolve to canvas-wide
    // passes the renderer can't nest (mirrors TrackKind::supports_transitions
    // in cutlass-models).
    if !matches!(
        track.kind,
        TrackKind::Video | TrackKind::Sticker | TrackKind::Text
    ) {
        return TransitionJunctionResolution {
            valid: false,
            left_clip_id: Default::default(),
            cut_tick: 0,
            row: hover_row,
        };
    }

    let mut spans: Vec<(SharedString, i32, i32)> = Vec::new();
    for idx in 0..track.clips.row_count() {
        let Some(clip) = track.clips.row_data(idx) else {
            continue;
        };
        let start = clip.timeline_start.value;
        let end = start.saturating_add(clip.source_range.duration.value);
        spans.push((clip.id.clone(), start, end));
    }
    spans.sort_by_key(|(_, start, _)| *start);

    let mut best_dist = snap_threshold_ticks + 1;
    let mut best_left = SharedString::default();
    let mut best_cut = 0;
    for window in spans.windows(2) {
        let (left_id, _, left_end) = &window[0];
        let (_, right_start, _) = &window[1];
        if left_end != right_start {
            continue;
        }
        let cut = *left_end;
        let dist = (cut - cursor_tick).abs();
        if dist <= snap_threshold_ticks && dist < best_dist {
            best_dist = dist;
            best_left = left_id.clone();
            best_cut = cut;
        }
    }

    if best_dist <= snap_threshold_ticks {
        TransitionJunctionResolution {
            valid: true,
            left_clip_id: best_left,
            cut_tick: best_cut,
            row: hover_row,
        }
    } else {
        TransitionJunctionResolution {
            valid: false,
            left_clip_id: Default::default(),
            cut_tick: 0,
            row: hover_row,
        }
    }
}

/// Lane-list row of the main track (the projection's `is-main` flag).
/// `None` without any video lane.
fn main_video_row(sequence: &Sequence) -> Option<i32> {
    (0..sequence.tracks.row_count()).find_map(|idx| {
        sequence
            .tracks
            .row_data(idx)
            .is_some_and(|t| t.is_main)
            .then_some(idx as i32)
    })
}

/// An insertion slot on the (gapless) main lane.
struct Insertion {
    /// Caret position in the lane's *current* visual space.
    display_tick: i32,
    /// Commit position handed to the worker. For reorders (`exclude` set)
    /// this is in the post-close arrangement — the worker closes the dragged
    /// clip's own gap before opening the new hole, shifting every later
    /// boundary left by the clip's duration.
    commit_tick: i32,
    /// The slot is exactly where the excluded clip already sits.
    noop: bool,
}

/// Pick the insertion slot for content whose left edge sits at `desired`:
/// before the first clip whose midpoint lies right of it (CapCut feel —
/// crossing a clip's middle flips the caret to its other side), else after
/// the last clip.
fn resolve_insertion(track: &crate::Track, exclude: Option<&str>, desired: i32) -> Insertion {
    // (start, end) spans of every clip except the excluded one. The
    // projection publishes clips in start order, but sort defensively —
    // lanes hold tens of clips, this runs once per drag frame.
    let mut spans: Vec<(i32, i32)> = Vec::new();
    let mut excluded: Option<(i32, i32)> = None; // (start, duration)
    for idx in 0..track.clips.row_count() {
        let Some(clip) = track.clips.row_data(idx) else {
            continue;
        };
        let start = clip.timeline_start.value;
        let dur = clip.source_range.duration.value;
        if exclude.is_some_and(|id| clip.id == id) {
            excluded = Some((start, dur));
        } else {
            spans.push((start, start.saturating_add(dur)));
        }
    }
    spans.sort_unstable();

    let index = spans
        .iter()
        .position(|(s, e)| desired < s + (e - s) / 2)
        .unwrap_or(spans.len());

    let (noop, ex_start, ex_dur) = match excluded {
        Some((start, dur)) => {
            let orig_index = spans.iter().filter(|(s, _)| *s < start).count();
            (index == orig_index, start, dur)
        }
        None => (false, 0, 0),
    };

    let display_tick = if noop {
        ex_start
    } else if index < spans.len() {
        spans[index].0
    } else {
        // After the last clip: the lane's current content end (the excluded
        // clip can't be last here, or the slot would be its own ⇒ noop).
        spans.iter().map(|(_, e)| *e).max().unwrap_or(0)
    };

    // Reorders commit against the closed arrangement: every boundary right
    // of the excluded clip's old start moves left by its duration.
    let closed = |tick: i32, anchor: i32| -> i32 {
        if excluded.is_some() && anchor > ex_start {
            tick - ex_dur
        } else {
            tick
        }
    };
    let commit_tick = if index < spans.len() {
        closed(spans[index].0, spans[index].0)
    } else {
        spans.iter().map(|(s, e)| closed(*e, *s)).max().unwrap_or(0)
    };

    Insertion {
        display_tick,
        commit_tick,
        noop,
    }
}

/// Resolve an edge-trim drag of `dx_ticks` (`trim_head` ⇔ the left edge).
///
/// The dragged edge magnets to clip edges / the playhead / tick 0, then
/// clamps to (in CapCut order of feel): the neighboring clips on the lane,
/// the source media headroom (`head-room-ticks` / `tail-room-ticks` from the
/// projection), tick 0, and a 1-tick minimum duration. A snap that the clamp
/// rejects is dropped (no guide line) rather than shown lying.
///
/// With `link_enabled`, the same edge delta applies to every clip in the
/// dragged clip's link group on release, so the clamp is the *intersection*
/// of every member's allowed delta range (each member's own neighbors,
/// headroom, and 1-tick minimum). The partners' post-trim extents come back
/// as `ghosts` for their stretch previews.
///
/// With `main_magnet` on, a trim on the main lane (bottom video track) skips
/// neighbor clamping — downstream clips ripple on commit instead.
#[allow(clippy::too_many_arguments)]
pub fn resolve_clip_trim(
    sequence: &Sequence,
    track_id: &str,
    clip_id: &str,
    trim_head: bool,
    dx_ticks: i32,
    playhead_tick: i32,
    snap_threshold_ticks: i32,
    link_enabled: bool,
    main_magnet: bool,
) -> ClipTrimResolution {
    let Some(ctx) = trim_context(sequence, track_id, clip_id) else {
        return ClipTrimResolution::invalid();
    };
    let old_end = ctx.start.saturating_add(ctx.duration);
    let old_edge = if trim_head { ctx.start } else { old_end };

    let partners = linked_partners(sequence, clip_id, link_enabled);
    let ripple = main_magnet && is_main_track(sequence, track_id);

    // Allowed edge-delta range: the dragged clip's bounds intersected with
    // every linked partner's. Delta 0 is inside each member's range, so the
    // intersection is never empty.
    let (mut delta_lo, mut delta_hi) = edge_delta_bounds(&ctx, trim_head, ripple);
    for (row, _, partner) in &partners {
        let partner_ripple = main_magnet
            && sequence
                .tracks
                .row_data(*row as usize)
                .is_some_and(|t| is_main_track(sequence, t.id.as_str()));
        let (lo, hi) = edge_delta_bounds(partner, trim_head, partner_ripple);
        delta_lo = delta_lo.max(lo);
        delta_hi = delta_hi.min(hi);
    }

    // Magnet the dragged edge alone: duration 0 turns the (start, end) pair
    // snap into a single-point snap.
    let desired = old_edge.saturating_add(dx_ticks);
    let snap = compute_drag_snap(
        sequence,
        track_id,
        clip_id,
        desired,
        0,
        snap_threshold_ticks,
        playhead_tick,
    );

    let delta = (snap.snapped_start_value - old_edge).clamp(delta_lo, delta_hi);
    let edge = old_edge + delta;
    let has_snap = snap.has_snap && edge == snap.snapped_start_value;

    let (new_start, new_duration) = if trim_head {
        (edge, old_end - edge)
    } else {
        (ctx.start, edge - ctx.start)
    };

    let ghosts: Vec<GroupGhost> = partners
        .iter()
        .map(|(row, color, p)| GroupGhost {
            row: *row,
            start_tick: if trim_head { p.start + delta } else { p.start },
            duration_ticks: if trim_head {
                p.duration - delta
            } else {
                p.duration + delta
            },
            color: *color,
        })
        .collect();

    ClipTrimResolution {
        valid: true,
        new_start,
        new_duration,
        has_snap,
        snap_line_tick: if has_snap { snap.snap_line_tick } else { 0 },
        is_noop: new_start == ctx.start && new_duration == ctx.duration,
        ghosts: ModelRc::from(Rc::new(VecModel::from(ghosts))),
    }
}

/// Allowed range for the dragged edge's delta on one clip: its lane
/// neighbors, source headroom, tick 0, and the 1-tick minimum, expressed
/// relative to the edge's current position. Always contains 0. On the main
/// lane with the magnet on, neighbor clamps are skipped (ripple on commit).
fn edge_delta_bounds(ctx: &TrimContext, trim_head: bool, ripple: bool) -> (i32, i32) {
    let end = ctx.start.saturating_add(ctx.duration);
    if trim_head {
        let lo = if ripple {
            ctx.start.saturating_sub(ctx.head_room).max(0)
        } else {
            ctx.prev_end
                .max(ctx.start.saturating_sub(ctx.head_room))
                .max(0)
        };
        (lo - ctx.start, (end - 1) - ctx.start)
    } else {
        let hi = if ripple {
            end.saturating_add(ctx.tail_room)
        } else {
            ctx.next_start.min(end.saturating_add(ctx.tail_room))
        };
        ((ctx.start + 1) - end, hi - end)
    }
}

fn is_main_track(sequence: &Sequence, track_id: &str) -> bool {
    main_video_row(sequence)
        .and_then(|row| sequence.tracks.row_data(row as usize))
        .is_some_and(|t| t.id == track_id)
}

/// `(lane row, lane color, trim context)` of every other clip in the dragged
/// clip's link group; empty while linkage is off or the clip is unlinked.
fn linked_partners(
    sequence: &Sequence,
    clip_id: &str,
    link_enabled: bool,
) -> Vec<(i32, slint::Color, TrimContext)> {
    if !link_enabled {
        return Vec::new();
    }
    let mut link = None;
    for idx in 0..sequence.tracks.row_count() {
        let Some(track) = sequence.tracks.row_data(idx) else {
            continue;
        };
        for clip_idx in 0..track.clips.row_count() {
            let Some(clip) = track.clips.row_data(clip_idx) else {
                continue;
            };
            if clip.id == clip_id && !clip.link_id.is_empty() {
                link = Some(clip.link_id.clone());
            }
        }
    }
    let Some(link) = link else {
        return Vec::new();
    };

    let mut partners = Vec::new();
    for idx in 0..sequence.tracks.row_count() {
        let Some(track) = sequence.tracks.row_data(idx) else {
            continue;
        };
        for clip_idx in 0..track.clips.row_count() {
            let Some(clip) = track.clips.row_data(clip_idx) else {
                continue;
            };
            if clip.link_id == link
                && clip.id != clip_id
                && let Some(ctx) = trim_context(sequence, track.id.as_str(), clip.id.as_str())
            {
                partners.push((idx as i32, track.color, ctx));
            }
        }
    }
    partners
}

/// The trimmed clip's placement, source headroom, and lane neighbors.
struct TrimContext {
    start: i32,
    duration: i32,
    head_room: i32,
    tail_room: i32,
    /// End of the nearest clip left of this one on the lane (0 if none).
    prev_end: i32,
    /// Start of the nearest clip right of this one (huge sentinel if none).
    next_start: i32,
}

fn trim_context(sequence: &Sequence, track_id: &str, clip_id: &str) -> Option<TrimContext> {
    let track = (0..sequence.tracks.row_count())
        .filter_map(|i| sequence.tracks.row_data(i))
        .find(|t| t.id == track_id)?;
    let clip = (0..track.clips.row_count())
        .filter_map(|i| track.clips.row_data(i))
        .find(|c| c.id == clip_id)?;

    let start = clip.timeline_start.value;
    let duration = clip.source_range.duration.value;
    let end = start.saturating_add(duration);

    let mut prev_end = 0;
    let mut next_start = i32::MAX / 2;
    for idx in 0..track.clips.row_count() {
        let Some(other) = track.clips.row_data(idx) else {
            continue;
        };
        if other.id == clip_id {
            continue;
        }
        let s = other.timeline_start.value;
        let e = s.saturating_add(other.source_range.duration.value);
        // Lane clips never overlap, so every other clip is fully on one side.
        if e <= start {
            prev_end = prev_end.max(e);
        }
        if s >= end {
            next_start = next_start.min(s);
        }
    }

    Some(TrimContext {
        start,
        duration,
        head_room: clip.head_room_ticks.max(0),
        tail_room: clip.tail_room_ticks.max(0),
        prev_end,
        next_start,
    })
}

/// `(track kind, timeline start, duration ticks)` of the dragged clip.
fn find_dragged_clip(
    sequence: &Sequence,
    source_track_id: &str,
    clip_id: &str,
) -> Option<(TrackKind, i32, i32)> {
    for track_idx in 0..sequence.tracks.row_count() {
        let track = sequence.tracks.row_data(track_idx)?;
        if track.id != source_track_id {
            continue;
        }
        for clip_idx in 0..track.clips.row_count() {
            let clip = track.clips.row_data(clip_idx)?;
            if clip.id == clip_id {
                return Some((
                    track.kind,
                    clip.timeline_start.value,
                    clip.source_range.duration.value,
                ));
            }
        }
    }
    None
}

/// Whether `[start, start + duration)` overlaps no clip on `track`
/// (excluding the dragged clip itself when moving within its own lane).
fn span_is_free(track: &crate::Track, start: i32, duration: i32, exclude: Option<&str>) -> bool {
    let end = start.saturating_add(duration);
    for clip_idx in 0..track.clips.row_count() {
        let Some(clip) = track.clips.row_data(clip_idx) else {
            continue;
        };
        if exclude.is_some_and(|id| clip.id == id) {
            continue;
        }
        let s = clip.timeline_start.value;
        let e = s.saturating_add(clip.source_range.duration.value);
        if start < e && s < end {
            return false;
        }
    }
    true
}

impl SnapResult {
    fn none(cursor: i32) -> Self {
        Self {
            has_snap: false,
            snapped_start_value: cursor,
            snap_line_tick: cursor,
        }
    }
}

impl ClipDragResolution {
    fn invalid() -> Self {
        Self {
            valid: false,
            is_new_lane: false,
            target_track_id: Default::default(),
            target_row: 0,
            resolved_start: 0,
            duration_ticks: 0,
            has_snap: false,
            snap_line_tick: 0,
            is_noop: true,
            is_insert: false,
            caret_tick: 0,
        }
    }
}

impl ClipTrimResolution {
    fn invalid() -> Self {
        Self {
            valid: false,
            new_start: 0,
            new_duration: 0,
            has_snap: false,
            snap_line_tick: 0,
            is_noop: true,
            ghosts: ModelRc::from(Rc::new(VecModel::<GroupGhost>::default())),
        }
    }
}

#[cfg(test)]
mod tests;
