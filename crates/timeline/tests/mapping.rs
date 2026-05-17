//! Time-mapping and clip-interval tests.

mod support;

use decoder::Rational;
use support::{project_three_clips, project_with_source, r, r_frac};
use timeline::{active_clip_on_track, Clip, ClipId, TimelineError, TrackKind};

#[test]
fn empty_track_returns_none() {
    let (p, track_id, _) = project_with_source("/a.mp4");
    assert!(
        p.active_clip_on_track(track_id, r(0))
            .unwrap()
            .is_none()
    );
}

#[test]
fn unknown_track_returns_error() {
    let p = timeline::Project::new();
    let err = p
        .active_clip_on_track(timeline::TrackId(999), r(0))
        .unwrap_err();
    assert!(matches!(err, TimelineError::TrackNotFound(_)));
}

#[test]
fn before_first_clip_is_none() {
    let (p, track_id, _, _) = project_three_clips();
    assert!(
        p.active_clip_on_track(track_id, r_frac(-1, 2))
            .unwrap()
            .is_none()
    );
}

#[test]
fn at_clip_start_is_inclusive() {
    let (p, track_id, source_id, ids) = project_three_clips();
    let active = p.active_clip_on_track(track_id, r(5)).unwrap().unwrap();
    assert_eq!(active.clip_id, ids[1]);
    assert_eq!(active.source_id, source_id);
    assert_eq!(active.media_time.reduced(), r(0));
}

#[test]
fn exclusive_end_first_clip_boundary() {
    let (p, track_id, _, ids) = project_three_clips();
    // Clip 0 is [0,5); t=5 belongs to clip 1.
    let active = p.active_clip_on_track(track_id, r(5)).unwrap().unwrap();
    assert_eq!(active.clip_id, ids[1]);
}

#[test]
fn just_before_clip_end_maps_correctly() {
    let (p, track_id, _, ids) = project_three_clips();
    let active = p
        .active_clip_on_track(track_id, r_frac(49, 10))
        .unwrap()
        .unwrap();
    assert_eq!(active.clip_id, ids[0]);
    assert_eq!(active.media_time.reduced(), r_frac(49, 10));
}

#[test]
fn gap_between_clips_is_none() {
    let (mut p, track_id, source_id) = project_with_source("/gap.mp4");
    support::add_clip(
        &mut p,
        track_id,
        support::clip_on_timeline(
            source_id,
            ClipId(1),
            r(0),
            r(0),
            r(2),
        ),
        true,
    )
    .unwrap();
    support::add_clip(
        &mut p,
        track_id,
        support::clip_on_timeline(
            source_id,
            ClipId(2),
            r(10),
            r(0),
            r(3),
        ),
        true,
    )
    .unwrap();
    assert!(
        p.active_clip_on_track(track_id, r(5))
            .unwrap()
            .is_none()
    );
}

#[test]
fn after_last_clip_is_none() {
    let (p, track_id, _, _) = project_three_clips();
    assert!(
        p.active_clip_on_track(track_id, r(100))
            .unwrap()
            .is_none()
    );
}

#[test]
fn media_time_with_nonzero_source_in() {
    let (mut p, track_id, source_id) = project_with_source("/offset.mp4");
    support::add_clip(
        &mut p,
        track_id,
        support::clip_on_timeline(
            source_id,
            ClipId(1),
            r(10),
            r(2),
            r(7),
        ),
        true,
    )
    .unwrap();
    let active = p.active_clip_on_track(track_id, r(12)).unwrap().unwrap();
    assert_eq!(active.media_time.reduced(), r(4));
}

#[test]
fn rational_denominators_mapping() {
    let (mut p, track_id, source_id) = project_with_source("/frac.mp4");
    support::add_clip(
        &mut p,
        track_id,
        support::clip_on_timeline(
            source_id,
            ClipId(1),
            r_frac(0, 1),
            r_frac(0, 1),
            r_frac(3, 1),
        ),
        true,
    )
    .unwrap();
    let t = r_frac(4, 3);
    let active = p.active_clip_on_track(track_id, t).unwrap().unwrap();
    assert_eq!(active.media_time.reduced(), t);
}

#[test]
fn many_clips_binary_search_all_hit() {
    let (mut p, track_id, source_id) = project_with_source("/many.mp4");
    for i in 0..32 {
        let pos = Rational::new_raw(i * 10, 1);
        support::add_clip(
            &mut p,
            track_id,
            support::clip_on_timeline(source_id, ClipId(i as u64), pos, r(0), r(5)),
            true,
        )
        .unwrap();
    }
    for i in 0..32 {
        let t = Rational::new_raw(i * 10 + 2, 1);
        let active = p.active_clip_on_track(track_id, t).unwrap().unwrap();
        assert_eq!(active.clip_id, ClipId(i as u64));
    }
}

#[test]
fn clip_contains_timeline_half_open() {
    let c = Clip {
        id: ClipId(0),
        source_id: timeline::MediaSourceId(0),
        source_in: r(0),
        source_out: r(5),
        timeline_position: r(0),
    };
    assert!(c.contains_timeline_time(r(0)));
    assert!(c.contains_timeline_time(r(4)));
    assert!(!c.contains_timeline_time(r(5)));
    assert!(!c.contains_timeline_time(r(-1)));
}

#[test]
fn clip_duration_and_timeline_end() {
    let c = Clip {
        id: ClipId(0),
        source_id: timeline::MediaSourceId(0),
        source_in: r_frac(1, 2),
        source_out: r_frac(9, 2),
        timeline_position: r_frac(10, 1),
    };
    assert_eq!(c.duration().unwrap().reduced(), r(4));
    assert_eq!(c.timeline_end().unwrap().reduced(), r(14));
}

#[test]
fn active_clip_via_track_slice_matches_project_api() {
    let (p, track_id, _, _) = project_three_clips();
    let track = p.track(track_id).unwrap();
    let direct = active_clip_on_track(track, r(9)).expect("hit");
    let via_project = p.active_clip_on_track(track_id, r(9)).unwrap().expect("hit");
    assert_eq!(direct, via_project);
}

#[test]
fn third_clip_media_time_at_end_minus_epsilon() {
    let (p, track_id, _, ids) = project_three_clips();
    let active = p
        .active_clip_on_track(track_id, r_frac(119, 10))
        .unwrap()
        .unwrap();
    assert_eq!(active.clip_id, ids[2]);
    assert_eq!(active.media_time.reduced(), r_frac(39, 10));
}

#[test]
fn exact_timeline_end_exclusive() {
    let (p, track_id, _, ids) = project_three_clips();
    assert!(
        p.active_clip_on_track(track_id, r(12))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        p.active_clip_on_track(track_id, r(11))
            .unwrap()
            .unwrap()
            .clip_id,
        ids[2]
    );
}

#[test]
fn negative_timeline_time_is_none() {
    let (p, track_id, _, _) = project_three_clips();
    assert!(
        p.active_clip_on_track(track_id, r(-1))
            .unwrap()
            .is_none()
    );
}

#[test]
fn track_kind_video_on_default_project() {
    let p = timeline::Project::new().with_default_video_track();
    let track_id = p.tracks[0].id;
    assert!(matches!(p.track(track_id).unwrap().kind, TrackKind::Video));
}
