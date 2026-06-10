//! Drag snap helpers operating on the Slint view model.
//!
//! While the user drags a clip, the gesture layer calls these every frame.
//! The engine will reuse the same policy once it owns project state.

use slint::Model;

use crate::{ResolvedTarget, Sequence, SnapResult};

pub fn compute_drag_snap(
    sequence: Sequence,
    dragging_source_track_id: &str,
    dragging_clip_id: &str,
    cursor_start_value: i32,
    clip_duration_ticks: i32,
    snap_threshold_ticks: i32,
) -> SnapResult {
    if snap_threshold_ticks <= 0 {
        return SnapResult::none(cursor_start_value);
    }

    let cursor_end = cursor_start_value.saturating_add(clip_duration_ticks);
    let mut best: Option<(i32, i32, i32)> = None;

    let mut consider = |candidate: i32| {
        let d_leading = (candidate - cursor_start_value).abs();
        if d_leading <= snap_threshold_ticks && best.map_or(true, |(d, _, _)| d_leading < d) {
            best = Some((d_leading, candidate, candidate));
        }
        let d_trailing = (candidate - cursor_end).abs();
        if d_trailing <= snap_threshold_ticks && best.map_or(true, |(d, _, _)| d_trailing < d) {
            let snapped_start = candidate.saturating_sub(clip_duration_ticks);
            best = Some((d_trailing, snapped_start, candidate));
        }
    };

    consider(0);

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

pub fn resolve_drag_target(
    sequence: Sequence,
    source_track_id: &str,
    lane_offset: i32,
) -> ResolvedTarget {
    let track_count = sequence.tracks.row_count();
    let Some(source_idx) = (0..track_count).find(|&i| {
        sequence
            .tracks
            .row_data(i)
            .is_some_and(|t| t.id == source_track_id)
    }) else {
        return ResolvedTarget::empty();
    };

    let Some(source_track) = sequence.tracks.row_data(source_idx) else {
        return ResolvedTarget::empty();
    };
    let source_kind = source_track.kind;

    let mut first = source_idx as i32;
    let mut last = source_idx as i32;
    for i in 0..track_count {
        let Some(track) = sequence.tracks.row_data(i) else {
            continue;
        };
        if track.kind == source_kind {
            let i_signed = i as i32;
            if i_signed < first {
                first = i_signed;
            }
            if i_signed > last {
                last = i_signed;
            }
        }
    }

    let raw = (source_idx as i32).saturating_add(lane_offset);
    let clamped_idx = raw.clamp(first, last);
    let clamped_offset = clamped_idx - source_idx as i32;

    ResolvedTarget {
        track_id: sequence
            .tracks
            .row_data(clamped_idx as usize)
            .map(|t| t.id)
            .unwrap_or_default(),
        clamped_offset,
    }
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

impl ResolvedTarget {
    fn empty() -> Self {
        Self {
            track_id: Default::default(),
            clamped_offset: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Clip, Rational, RationalTime, Sequence, TimeRange, Track, TrackKind};
    use slint::{ModelRc, SharedString, VecModel};
    use std::rc::Rc;

    fn rt(value: i32) -> RationalTime {
        RationalTime {
            value,
            rate: Rational { num: 24, den: 1 },
        }
    }

    fn sample_sequence() -> Sequence {
        let clip = |id: &str, start: i32, dur: i32| Clip {
            id: SharedString::from(id),
            name: SharedString::from(id),
            timeline_start: rt(start),
            source_range: TimeRange {
                start: rt(0),
                duration: rt(dur),
            },
        };
        let tracks = vec![
            Track {
                id: SharedString::from("1"),
                name: SharedString::from("V1"),
                kind: TrackKind::Video,
                color: slint::Color::from_rgb_u8(0x4A, 0x6F, 0xA5),
                clips: ModelRc::from(Rc::new(VecModel::from(vec![clip("1", 10, 100)]))),
            },
            Track {
                id: SharedString::from("2"),
                name: SharedString::from("V2"),
                kind: TrackKind::Video,
                color: slint::Color::from_rgb_u8(0x5E, 0x8B, 0x7E),
                clips: ModelRc::from(Rc::new(VecModel::from(vec![
                    clip("2", 0, 80),
                    clip("3", 120, 60),
                ]))),
            },
        ];
        Sequence {
            id: SharedString::from("1"),
            name: SharedString::from("Sequence 1"),
            fps: Rational { num: 24, den: 1 },
            drop_frame: false,
            tracks: ModelRc::from(Rc::new(VecModel::from(tracks))),
            width: 1080.0,
            height: 1920.0,
        }
    }

    #[test]
    fn zero_offset_returns_source_lane() {
        let seq = sample_sequence();
        let r = resolve_drag_target(seq, "2", 0);
        assert_eq!(r.track_id, "2");
        assert_eq!(r.clamped_offset, 0);
    }

    #[test]
    fn snaps_to_zero_origin_when_dragging_near_start() {
        let seq = sample_sequence();
        let r = compute_drag_snap(seq, "1", "1", 3, 100, 5);
        assert!(r.has_snap);
        assert_eq!(r.snapped_start_value, 0);
    }
}
