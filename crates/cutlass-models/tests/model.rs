use cutlass_models::{
    Generator, MediaSource, ModelError, Project, Rational, Shape, TimeRange, TrackKind,
};

fn sample_media(fps: Rational, duration: i64) -> MediaSource {
    MediaSource::new("/tmp/sample.mp4", 3840, 2160, fps, duration, true)
}

#[test]
fn build_project_and_query_by_id() {
    let mut project = Project::new("demo", Rational::FPS_24);

    let media = sample_media(Rational::FPS_24, 1000);
    let media_id = project.add_media(media);

    let v1 = project.add_track(TrackKind::Video, "V1");

    // Two non-overlapping clips on the same track.
    let c1 = project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 0)
        .expect("first clip");
    let c2 = project
        .add_clip(v1, media_id, TimeRange::new(200, 100), 100)
        .expect("second clip");

    // O(1) lookup by id across the timeline.
    assert_eq!(
        project.clip(c1).unwrap().source_range(),
        Some(TimeRange::new(0, 100))
    );
    assert_eq!(project.clip(c1).unwrap().media(), Some(media_id));
    assert_eq!(project.clip(c2).unwrap().start(), 100);
    assert_eq!(project.timeline().track_of(c1), Some(v1));

    // Timeline duration = end of the last clip.
    assert_eq!(project.timeline().duration(), 200);
    assert_eq!(project.timeline().clip_count(), 2);
}

#[test]
fn generated_clips_need_no_media() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let title = project.add_track(TrackKind::Video, "Titles");

    // A text and a shape clip, neither backed by media.
    let text = project
        .add_generated(
            title,
            Generator::Text {
                content: "Hello".into(),
            },
            TimeRange::new(0, 48),
        )
        .unwrap();
    let shape = project
        .add_generated(
            title,
            Generator::Shape {
                shape: Shape::Rectangle,
            },
            TimeRange::new(48, 48),
        )
        .unwrap();

    assert_eq!(project.clip(text).unwrap().media(), None);
    assert!(project.clip(text).unwrap().is_generated());
    assert_eq!(project.clip(shape).unwrap().source_range(), None);
    assert_eq!(project.media_count(), 0);
    assert_eq!(project.timeline().duration(), 96);
}

#[test]
fn overlap_is_rejected() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");

    project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 0)
        .unwrap();
    let err = project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 50)
        .unwrap_err();
    assert_eq!(err, ModelError::Overlap(v1));
}

#[test]
fn unknown_refs_error() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let v1 = project.add_track(TrackKind::Video, "V1");
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));

    // Unknown media.
    let bad_media = MediaSource::new("/x", 1, 1, Rational::FPS_24, 10, false).id;
    assert!(matches!(
        project.add_clip(v1, bad_media, TimeRange::new(0, 10), 0),
        Err(ModelError::UnknownMedia(_))
    ));

    // Source range past the media bounds.
    assert_eq!(
        project.add_clip(v1, media_id, TimeRange::new(900, 200), 0),
        Err(ModelError::SourceOutOfBounds)
    );
}

#[test]
fn rate_conform_adjusts_timeline_duration() {
    // 30fps source on a 24fps timeline: 120 source frames (4s) -> 96 timeline frames.
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_30, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");

    let clip_id = project
        .add_clip(v1, media_id, TimeRange::new(0, 120), 0)
        .unwrap();
    assert_eq!(project.clip(clip_id).unwrap().timeline.duration, 96);
}

#[test]
fn removing_referenced_media_fails_then_succeeds() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let clip_id = project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 0)
        .unwrap();

    assert_eq!(
        project.remove_media(media_id),
        Err(ModelError::MediaReferenced(media_id))
    );

    project.timeline_mut().remove_clip(clip_id).unwrap();
    assert!(project.remove_media(media_id).is_ok());
    assert_eq!(project.media_count(), 0);
}

#[test]
fn track_stacking_order_is_preserved() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let v1 = project.add_track(TrackKind::Video, "V1");
    let v2 = project.add_track(TrackKind::Video, "V2");
    let a1 = project.add_track(TrackKind::Audio, "A1");

    assert_eq!(project.timeline().order(), &[v1, v2, a1]);
    let names: Vec<&str> = project
        .timeline()
        .tracks_ordered()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(names, ["V1", "V2", "A1"]);
}

#[test]
fn clip_at_and_source_mapping() {
    // Same-rate (24/24) so source duration maps 1:1 to timeline frames:
    // source [100,110) placed at timeline_start=10 -> timeline [10,20).
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let id = project
        .add_clip(v1, media_id, TimeRange::new(100, 10), 10)
        .unwrap();

    let track = project.timeline().track(v1).unwrap();
    assert_eq!(track.clip_at(15).map(|c| c.id), Some(id));
    assert!(track.clip_at(25).is_none());
    assert_eq!(project.clip(id).unwrap().source_frame_at(15), Some(105));
}

#[test]
fn split_media_clip_divides_timeline_and_source() {
    // Same-rate so source and timeline frames line up 1:1.
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    // source [100, 200) at timeline 0 -> timeline [0, 100).
    let left = project
        .add_clip(v1, media_id, TimeRange::new(100, 100), 0)
        .unwrap();

    let right = project.split_clip(left, 40).expect("split inside the clip");
    assert_ne!(left, right);

    // Left keeps [0,40) of the timeline and [100,140) of the source.
    let l = project.clip(left).unwrap();
    assert_eq!(l.timeline, TimeRange::new(0, 40));
    assert_eq!(l.source_range(), Some(TimeRange::new(100, 40)));
    // Right takes [40,100) / [140,200).
    let r = project.clip(right).unwrap();
    assert_eq!(r.timeline, TimeRange::new(40, 60));
    assert_eq!(r.source_range(), Some(TimeRange::new(140, 60)));
    // The two halves still cover the original extent with no gap or overlap.
    assert_eq!(project.timeline().duration(), 100);
    assert_eq!(project.timeline().clip_count(), 2);
}

#[test]
fn split_at_or_outside_boundary_is_rejected() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let clip = project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 10)
        .unwrap();

    assert_eq!(project.split_clip(clip, 10), Err(ModelError::InvalidRange)); // at start
    assert_eq!(project.split_clip(clip, 110), Err(ModelError::InvalidRange)); // at end
    assert_eq!(project.split_clip(clip, 200), Err(ModelError::InvalidRange)); // past end
}

#[test]
fn trim_head_advances_source_in_point() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    // source [100,200) at timeline [0,100).
    let clip = project
        .add_clip(v1, media_id, TimeRange::new(100, 100), 0)
        .unwrap();

    // Trim the head in by 30 frames: timeline [30,100), source [130,200).
    project
        .trim_clip(clip, TimeRange::new(30, 70))
        .expect("head trim within bounds");
    let c = project.clip(clip).unwrap();
    assert_eq!(c.timeline, TimeRange::new(30, 70));
    assert_eq!(c.source_range(), Some(TimeRange::new(130, 70)));
}

#[test]
fn trim_past_source_bounds_is_rejected() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 100));
    let v1 = project.add_track(TrackKind::Video, "V1");
    // source [90,100) at timeline [0,10) — only 10 source frames left at the tail.
    let clip = project
        .add_clip(v1, media_id, TimeRange::new(90, 10), 0)
        .unwrap();

    // Extending the tail to 40 frames would need source past frame 100.
    assert_eq!(
        project.trim_clip(clip, TimeRange::new(0, 40)),
        Err(ModelError::SourceOutOfBounds)
    );
}

#[test]
fn trim_into_neighbour_is_rejected() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let a = project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 0)
        .unwrap();
    project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 100)
        .unwrap();

    // Growing clip `a` from [0,100) to [0,150) would collide with the neighbour.
    assert_eq!(
        project.trim_clip(a, TimeRange::new(0, 150)),
        Err(ModelError::Overlap(v1))
    );
}

#[test]
fn move_clip_across_tracks_and_rejects_overlap() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let v2 = project.add_track(TrackKind::Video, "V2");
    let clip = project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 0)
        .unwrap();
    // An occupant on v2 blocking [0,100).
    project
        .add_clip(v2, media_id, TimeRange::new(0, 100), 0)
        .unwrap();

    // Colliding move is rejected and changes nothing.
    assert_eq!(project.move_clip(clip, v2, 0), Err(ModelError::Overlap(v2)));
    assert_eq!(project.timeline().track_of(clip), Some(v1));

    // Moving to a free slot on v2 succeeds, keeping the clip's duration.
    project.move_clip(clip, v2, 200).unwrap();
    assert_eq!(project.timeline().track_of(clip), Some(v2));
    assert_eq!(project.clip(clip).unwrap().timeline, TimeRange::new(200, 100));
}

#[test]
fn ripple_delete_closes_the_gap() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let a = project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 0)
        .unwrap();
    let b = project
        .add_clip(v1, media_id, TimeRange::new(100, 100), 100)
        .unwrap();
    let c = project
        .add_clip(v1, media_id, TimeRange::new(200, 100), 200)
        .unwrap();

    // Delete the middle clip; later clips slide left by its 100-frame duration.
    project.ripple_delete(b).unwrap();
    assert!(project.clip(b).is_none());
    assert_eq!(project.clip(a).unwrap().start(), 0); // before the gap, unchanged
    assert_eq!(project.clip(c).unwrap().start(), 100); // shifted from 200 -> 100
    assert_eq!(project.timeline().duration(), 200);
}

#[test]
fn editing_unknown_clip_errors() {
    let mut project = Project::new("demo", Rational::FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let clip = project
        .add_clip(v1, media_id, TimeRange::new(0, 100), 0)
        .unwrap();
    let gone = project.ripple_delete(clip).unwrap().id;

    assert!(matches!(
        project.split_clip(gone, 5),
        Err(ModelError::UnknownClip(_))
    ));
    assert!(matches!(
        project.trim_clip(gone, TimeRange::new(0, 10)),
        Err(ModelError::UnknownClip(_))
    ));
    assert!(matches!(
        project.move_clip(gone, v1, 0),
        Err(ModelError::UnknownClip(_))
    ));
}
