//! Validation, overlap detection, and invariant tests.

mod support;

use support::{
    add_clip, assert_snapshot_unchanged, clip_on_timeline, empty_video_project, lock_track,
    project_with_source, r, set_probed, snapshot,
};
use timeline::{ClipId, TimelineError, TrackId};

#[test]
fn zero_duration_clip_rejected() {
    let (mut p, track_id, source_id) = project_with_source("/z.mp4");
    let before = snapshot(&p);
    let err = add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(1), r(0), r(5), r(5)),
        true,
    )
    .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidTrim(_)));
    assert_snapshot_unchanged(&p, &before);
}

#[test]
fn inverted_source_range_rejected() {
    let (mut p, track_id, source_id) = project_with_source("/inv.mp4");
    let before = snapshot(&p);
    let err = add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(1), r(0), r(10), r(3)),
        true,
    )
    .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidTrim(_)));
    assert_snapshot_unchanged(&p, &before);
}

#[test]
fn missing_source_rejected() {
    let (mut p, track_id) = empty_video_project();
    let before = snapshot(&p);
    let err = add_clip(
        &mut p,
        track_id,
        clip_on_timeline(timeline::MediaSourceId(99), ClipId(1), r(0), r(0), r(5)),
        true,
    )
    .unwrap_err();
    assert!(matches!(err, TimelineError::SourceNotFound(_)));
    assert_snapshot_unchanged(&p, &before);
}

#[test]
fn source_out_beyond_probed_duration_rejected() {
    let (mut p, track_id, source_id) = project_with_source("/dur.mp4");
    set_probed(&mut p, source_id, 20);
    let before = snapshot(&p);
    let err = add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(1), r(0), r(0), r(25)),
        true,
    )
    .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidTrim(_)));
    assert_snapshot_unchanged(&p, &before);
}

#[test]
fn adjacent_clips_touching_allowed() {
    let (mut p, track_id, source_id) = project_with_source("/adj.mp4");
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(1), r(0), r(0), r(5)),
        true,
    )
    .unwrap();
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(2), r(5), r(0), r(3)),
        true,
    )
    .unwrap();
    assert_eq!(p.track(track_id).unwrap().clips.len(), 2);
}

#[test]
fn partial_overlap_rejected() {
    let (mut p, track_id, source_id) = project_with_source("/ov.mp4");
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(1), r(0), r(0), r(10)),
        true,
    )
    .unwrap();
    let before = snapshot(&p);
    let err = add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(99), r(7), r(0), r(5)),
        true,
    )
    .unwrap_err();
    assert!(matches!(err, TimelineError::ClipOverlap { .. }));
    assert_snapshot_unchanged(&p, &before);
}

#[test]
fn containment_overlap_rejected() {
    let (mut p, track_id, source_id) = project_with_source("/contain.mp4");
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(1), r(0), r(0), r(20)),
        true,
    )
    .unwrap();
    let err = add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(2), r(5), r(0), r(5)),
        true,
    )
    .unwrap_err();
    assert!(matches!(err, TimelineError::ClipOverlap { .. }));
}

#[test]
fn insert_maintains_sorted_order() {
    let (mut p, track_id, source_id) = project_with_source("/sort.mp4");
    for (id, pos) in [(3, 30), (1, 0), (2, 10)] {
        add_clip(
            &mut p,
            track_id,
            clip_on_timeline(source_id, ClipId(id), r(pos), r(0), r(5)),
            true,
        )
        .unwrap();
    }
    let positions: Vec<_> = p.track(track_id).unwrap().clips.iter().map(|c| c.id).collect();
    assert_eq!(positions, vec![ClipId(1), ClipId(2), ClipId(3)]);
}

#[test]
fn locked_track_rejects_insert() {
    let (mut p, track_id, source_id) = project_with_source("/lock.mp4");
    lock_track(&mut p, track_id);
    let err = add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(1), r(0), r(0), r(5)),
        true,
    )
    .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidTrim(_)));
}

#[test]
fn unknown_track_insert_rejected() {
    let (mut p, _, source_id) = project_with_source("/t.mp4");
    let err = p
        .insert_clip(TrackId(999), clip_on_timeline(source_id, ClipId(1), r(0), r(0), r(5)))
        .unwrap_err();
    assert!(matches!(err, TimelineError::TrackNotFound(_)));
}

#[test]
fn remove_unknown_clip_errors() {
    let (mut p, _, _, _) = support::project_three_clips();
    let err = p.remove_clip(ClipId(404)).unwrap_err();
    assert!(matches!(err, TimelineError::ClipNotFound(_)));
}

#[test]
fn clip_lookup_returns_parent_track() {
    let (p, track_id, _, ids) = support::project_three_clips();
    let (track, clip) = p.clip(ids[1]).unwrap();
    assert_eq!(track.id, track_id);
    assert_eq!(clip.id, ids[1]);
}
