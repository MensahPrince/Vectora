//! Command apply / undo / redo coverage.

mod support;

use support::{
    add_clip, clip_on_timeline, empty_video_project, lock_track, path, project_three_clips,
    project_with_source, r, snapshot,
};
use timeline::{
    AddClip, AddSource, AddTrack, ClipId, Command, MediaSourceId, MoveClip, Project, RemoveClip,
    RemoveSource, RemoveTrack, SetSourceProbed, TimelineError, TrackKind, TrimClipIn, TrimClipOut,
};

#[test]
fn add_source_allocates_monotonic_ids() {
    let mut p = Project::new();
    p.apply(Box::new(AddSource::new("/a.mp4")), true).unwrap();
    p.apply(Box::new(AddSource::new("/b.mp4")), true).unwrap();
    let mut ids: Vec<_> = p.sources.keys().copied().collect();
    ids.sort_by_key(|s| s.0);
    assert_eq!(ids, vec![MediaSourceId(0), MediaSourceId(1)]);
}

#[test]
fn add_source_undo_removes_only_when_unused() {
    let (mut p, track_id) = empty_video_project();
    p.apply(Box::new(AddSource::new("/a.mp4")), true).unwrap();
    let sid = MediaSourceId(0);
    let clip = clip_on_timeline(sid, ClipId(1), r(0), r(0), r(5));
    p.apply(Box::new(AddClip::new(track_id, clip)), true).unwrap();
    p.undo().unwrap(); // undo add clip
    p.undo().unwrap(); // undo add source
    assert!(p.sources.is_empty());
}

#[test]
fn add_source_undo_skips_remove_while_clip_still_on_timeline() {
    let (mut p, track_id) = empty_video_project();
    p.apply(Box::new(AddSource::new("/a.mp4")), true).unwrap();
    let sid = MediaSourceId(0);
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(sid, ClipId(1), r(0), r(0), r(5)),
        true,
    )
    .unwrap();
    // Undo only AddClip; AddSource undo must not run yet — source still referenced.
    p.undo().unwrap();
    assert!(p.sources.contains_key(&sid));
    assert!(p.clip(ClipId(1)).is_err());
}

#[test]
fn remove_source_after_clips_gone() {
    let (mut p, track_id, source_id) = project_with_source("/rm.mp4");
    let clip_id = ClipId(1);
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, clip_id, r(0), r(0), r(5)),
        true,
    )
    .unwrap();
    p.apply(Box::new(RemoveClip::new(clip_id)), true).unwrap();
    p.apply(Box::new(RemoveSource::new(source_id)), true)
        .unwrap();
    assert!(!p.sources.contains_key(&source_id));
    p.undo().unwrap();
    assert!(p.sources.contains_key(&source_id));
}

#[test]
fn remove_source_missing_errors() {
    let mut p = Project::new();
    let err = p
        .apply(Box::new(RemoveSource::new(MediaSourceId(0))), true)
        .unwrap_err();
    assert!(matches!(err, TimelineError::SourceNotFound(_)));
}

#[test]
fn move_clip_to_valid_gap() {
    let (mut p, _, _, ids) = project_three_clips();
    let before = snapshot(&p);
    p.apply(Box::new(MoveClip::new(ids[0], r(20))), true)
        .unwrap();
    assert_ne!(before, snapshot(&p));
    let pos = p.clip(ids[0]).unwrap().1.timeline_position;
    assert_eq!(pos.reduced(), r(20));
}

#[test]
fn move_clip_overlap_rejected() {
    let (mut p, _, _, ids) = project_three_clips();
    let before = snapshot(&p);
    let err = p
        .apply(Box::new(MoveClip::new(ids[0], r(6))), true)
        .unwrap_err();
    assert!(matches!(err, TimelineError::ClipOverlap { .. }));
    assert_eq!(before, snapshot(&p));
}

#[test]
fn move_clip_locked_track_rejected() {
    let (mut p, track_id, source_id) = project_with_source("/mvlock.mp4");
    let clip_id = ClipId(1);
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, clip_id, r(0), r(0), r(5)),
        true,
    )
    .unwrap();
    lock_track(&mut p, track_id);
    let err = p
        .apply(Box::new(MoveClip::new(clip_id, r(10))), true)
        .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidTrim(_)));
}

#[test]
fn move_clip_unknown_id_errors() {
    let (mut p, _, _, _) = project_three_clips();
    let err = p
        .apply(Box::new(MoveClip::new(ClipId(404), r(0))), true)
        .unwrap_err();
    assert!(matches!(err, TimelineError::ClipNotFound(_)));
}

#[test]
fn move_clip_undo_redo() {
    let (mut p, _, _, ids) = project_three_clips();
    let before = snapshot(&p);
    p.apply(Box::new(MoveClip::new(ids[1], r(50))), true)
        .unwrap();
    p.undo().unwrap();
    assert_eq!(before, snapshot(&p));
    p.redo().unwrap();
    assert_ne!(before, snapshot(&p));
}

#[test]
fn trim_in_extending_media_shifts_timeline_left() {
    let (mut p, track_id, source_id) = project_with_source("/extend.mp4");
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(source_id, ClipId(1), r(10), r(2), r(8)),
        true,
    )
    .unwrap();
    p.apply(
        Box::new(TrimClipIn::new(ClipId(1), r(0))),
        true,
    )
    .unwrap();
    let clip = p.clip(ClipId(1)).unwrap().1;
    assert_eq!(clip.source_in.reduced(), r(0));
    assert_eq!(clip.timeline_position.reduced(), r(8));
}

#[test]
fn trim_in_shifts_timeline_position() {
    let (mut p, _, _, ids) = project_three_clips();
    p.apply(
        Box::new(TrimClipIn::new(ids[0], r(2))),
        true,
    )
    .unwrap();
    let clip = p.clip(ids[0]).unwrap().1;
    assert_eq!(clip.source_in.reduced(), r(2));
    assert_eq!(clip.timeline_position.reduced(), r(2));
}

#[test]
fn trim_in_invalid_in_ge_out() {
    let (mut p, _, _, ids) = project_three_clips();
    let before = snapshot(&p);
    let err = p
        .apply(
            Box::new(TrimClipIn::new(ids[0], r(10))),
            true,
        )
        .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidTrim(_)));
    assert_eq!(before, snapshot(&p));
}

#[test]
fn trim_out_overlap_rejected() {
    let (mut p, track_id, source_id) = project_with_source("/trimov.mp4");
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
        clip_on_timeline(source_id, ClipId(2), r(5), r(0), r(5)),
        true,
    )
    .unwrap();
    let err = p
        .apply(
            Box::new(TrimClipOut::new(ClipId(1), r(8))),
            true,
        )
        .unwrap_err();
    assert!(matches!(err, TimelineError::ClipOverlap { .. }));
}

#[test]
fn trim_out_shortens_duration() {
    let (mut p, _, _, ids) = project_three_clips();
    p.apply(
        Box::new(TrimClipOut::new(ids[2], r(2))),
        true,
    )
    .unwrap();
    let clip = p.clip(ids[2]).unwrap().1;
    assert_eq!(clip.source_out.reduced(), r(2));
    assert_eq!(clip.duration().unwrap().reduced(), r(2));
}

#[test]
fn trim_out_invalid_out_le_in() {
    let (mut p, _, _, ids) = project_three_clips();
    let err = p
        .apply(
            Box::new(TrimClipOut::new(ids[0], r(0))),
            true,
        )
        .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidTrim(_)));
}

#[test]
fn trim_out_redo_round_trip() {
    let (mut p, _, _, ids) = project_three_clips();
    let before = snapshot(&p);
    p.apply(
        Box::new(TrimClipOut::new(ids[1], r(2))),
        true,
    )
    .unwrap();
    p.undo().unwrap();
    assert_eq!(before, snapshot(&p));
    p.redo().unwrap();
    assert_ne!(before, snapshot(&p));
}

#[test]
fn remove_track_with_clips_fails() {
    let (p, track_id, _, _) = project_three_clips();
    let mut p = p;
    let err = p
        .apply(Box::new(RemoveTrack::new(track_id)), true)
        .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidTrim(_)));
}

#[test]
fn remove_track_empty_succeeds() {
    let (mut p, track_id) = empty_video_project();
    p.apply(Box::new(RemoveTrack::new(track_id)), true)
        .unwrap();
    assert!(p.tracks.is_empty());
}

#[test]
fn add_track_video_kind() {
    let mut p = Project::new();
    p.apply(Box::new(AddTrack::new(TrackKind::Video)), true)
        .unwrap();
    assert_eq!(p.tracks.len(), 1);
    assert!(matches!(p.tracks[0].kind, TrackKind::Video));
}

#[test]
fn set_source_probed_updates_and_undo_via_command() {
    let (mut p, _, source_id) = project_with_source("/probe.mp4");
    let info = support::probed_info(60);
    let mut cmd = SetSourceProbed::new(source_id, info.clone());
    cmd.apply(&mut p).unwrap();
    assert_eq!(p.source(source_id).unwrap().probed.as_ref(), Some(&info));
    cmd.undo(&mut p);
    assert!(p.source(source_id).unwrap().probed.is_none());
}

#[test]
fn set_source_probed_unknown_source_errors() {
    let mut p = Project::new();
    let err = p
        .apply(
            Box::new(SetSourceProbed::new(
                MediaSourceId(0),
                support::probed_info(10),
            )),
            false,
        )
        .unwrap_err();
    assert!(matches!(err, TimelineError::SourceNotFound(_)));
}

#[test]
fn remove_clip_preserves_order_of_remaining() {
    let (mut p, track_id, _, ids) = project_three_clips();
    p.apply(Box::new(RemoveClip::new(ids[1])), true).unwrap();
    let remaining: Vec<_> = p.track(track_id).unwrap().clips.iter().map(|c| c.id).collect();
    assert_eq!(remaining, vec![ids[0], ids[2]]);
}

#[test]
fn clips_using_source_lists_all_references() {
    let (p, _, source_id, ids) = project_three_clips();
    let mut used = p.clips_using_source(source_id);
    used.sort_by_key(|c| c.0);
    let mut expected = ids.to_vec();
    expected.sort_by_key(|c| c.0);
    assert_eq!(used, expected);
}

#[test]
fn multiple_sources_independent() {
    let (mut p, track_id) = empty_video_project();
    p.apply(Box::new(AddSource::new(path("/a.mp4"))), true)
        .unwrap();
    p.apply(Box::new(AddSource::new(path("/b.mp4"))), true)
        .unwrap();
    let s0 = MediaSourceId(0);
    let s1 = MediaSourceId(1);
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(s0, ClipId(1), r(0), r(0), r(3)),
        true,
    )
    .unwrap();
    add_clip(
        &mut p,
        track_id,
        clip_on_timeline(s1, ClipId(2), r(10), r(0), r(3)),
        true,
    )
    .unwrap();
    assert_eq!(p.sources.len(), 2);
    assert_eq!(p.clips_using_source(s0), vec![ClipId(1)]);
}
