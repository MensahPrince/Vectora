use cutlass_models::{
    Generator, MediaSource, ModelError, Project, Rational, RationalTime, Shape, TimeRange,
    TrackKind,
};

const FPS_24: Rational = Rational::FPS_24;

fn rt(value: i64) -> RationalTime {
    RationalTime::new(value, FPS_24)
}

fn tr(start: i64, duration: i64) -> TimeRange {
    TimeRange::at_rate(start, duration, FPS_24)
}

fn tr_at(start: i64, duration: i64, rate: Rational) -> TimeRange {
    TimeRange::at_rate(start, duration, rate)
}

fn sample_media(fps: Rational, duration: i64) -> MediaSource {
    MediaSource::new("/tmp/sample.mp4", 3840, 2160, fps, duration, true)
}

#[test]
fn build_project_and_query_by_id() {
    let mut project = Project::new("demo", FPS_24);

    let media = sample_media(FPS_24, 1000);
    let media_id = project.add_media(media);

    let v1 = project.add_track(TrackKind::Video, "V1");

    let c1 = project
        .add_clip(v1, media_id, tr(0, 100), rt(0))
        .expect("first clip");
    let c2 = project
        .add_clip(v1, media_id, tr(200, 100), rt(100))
        .expect("second clip");

    assert_eq!(
        project.clip(c1).unwrap().source_range(),
        Some(tr(0, 100))
    );
    assert_eq!(project.clip(c1).unwrap().media(), Some(media_id));
    assert_eq!(project.clip(c2).unwrap().start().value, 100);
    assert_eq!(project.timeline().track_of(c1), Some(v1));

    assert_eq!(project.timeline().duration().value, 200);
    assert_eq!(project.timeline().clip_count(), 2);
}

#[test]
fn generated_clips_need_no_media() {
    let mut project = Project::new("demo", FPS_24);
    let title = project.add_track(TrackKind::Video, "Titles");

    let text = project
        .add_generated(
            title,
            Generator::Text {
                content: "Hello".into(),
            },
            tr(0, 48),
        )
        .unwrap();
    let shape = project
        .add_generated(
            title,
            Generator::Shape {
                shape: Shape::Rectangle,
            },
            tr(48, 48),
        )
        .unwrap();

    assert_eq!(project.clip(text).unwrap().media(), None);
    assert!(project.clip(text).unwrap().is_generated());
    assert_eq!(project.clip(shape).unwrap().source_range(), None);
    assert_eq!(project.media_count(), 0);
    assert_eq!(project.timeline().duration().value, 96);
}

#[test]
fn overlap_is_rejected() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");

    project.add_clip(v1, media_id, tr(0, 100), rt(0)).unwrap();
    let err = project
        .add_clip(v1, media_id, tr(0, 100), rt(50))
        .unwrap_err();
    assert_eq!(err, ModelError::Overlap(v1));
}

#[test]
fn unknown_refs_error() {
    let mut project = Project::new("demo", FPS_24);
    let v1 = project.add_track(TrackKind::Video, "V1");
    let media_id = project.add_media(sample_media(FPS_24, 1000));

    let bad_media = MediaSource::new("/x", 1, 1, FPS_24, 10, false).id;
    assert!(matches!(
        project.add_clip(v1, bad_media, tr(0, 10), rt(0)),
        Err(ModelError::UnknownMedia(_))
    ));

    assert_eq!(
        project.add_clip(v1, media_id, tr(900, 200), rt(0)),
        Err(ModelError::SourceOutOfBounds)
    );
}

#[test]
fn rate_conform_adjusts_timeline_duration() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(Rational::FPS_30, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");

    let clip_id = project
        .add_clip(
            v1,
            media_id,
            tr_at(0, 120, Rational::FPS_30),
            rt(0),
        )
        .unwrap();
    assert_eq!(project.clip(clip_id).unwrap().timeline.duration.value, 96);
}

#[test]
fn removing_referenced_media_fails_then_succeeds() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let clip_id = project
        .add_clip(v1, media_id, tr(0, 100), rt(0))
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
    let mut project = Project::new("demo", FPS_24);
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
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let id = project
        .add_clip(v1, media_id, tr(100, 10), rt(10))
        .unwrap();

    let track = project.timeline().track(v1).unwrap();
    assert_eq!(
        track.clip_at(rt(15)).unwrap().map(|c| c.id),
        Some(id)
    );
    assert!(track.clip_at(rt(25)).unwrap().is_none());
    assert_eq!(
        project
            .clip(id)
            .unwrap()
            .source_time_at(rt(15))
            .unwrap()
            .map(|t| t.value),
        Some(105)
    );
}

#[test]
fn split_media_clip_divides_timeline_and_source() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let left = project
        .add_clip(v1, media_id, tr(100, 100), rt(0))
        .unwrap();

    let right = project.split_clip(left, rt(40)).expect("split inside the clip");
    assert_ne!(left, right);

    let l = project.clip(left).unwrap();
    assert_eq!(l.timeline, tr(0, 40));
    assert_eq!(l.source_range(), Some(tr(100, 40)));
    let r = project.clip(right).unwrap();
    assert_eq!(r.timeline, tr(40, 60));
    assert_eq!(r.source_range(), Some(tr(140, 60)));
    assert_eq!(project.timeline().duration().value, 100);
    assert_eq!(project.timeline().clip_count(), 2);
}

#[test]
fn split_at_or_outside_boundary_is_rejected() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let clip = project
        .add_clip(v1, media_id, tr(0, 100), rt(10))
        .unwrap();

    assert_eq!(project.split_clip(clip, rt(10)), Err(ModelError::InvalidRange));
    assert_eq!(project.split_clip(clip, rt(110)), Err(ModelError::InvalidRange));
    assert_eq!(project.split_clip(clip, rt(200)), Err(ModelError::InvalidRange));
}

#[test]
fn trim_head_advances_source_in_point() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let clip = project
        .add_clip(v1, media_id, tr(100, 100), rt(0))
        .unwrap();

    project
        .trim_clip(clip, tr(30, 70))
        .expect("head trim within bounds");
    let c = project.clip(clip).unwrap();
    assert_eq!(c.timeline, tr(30, 70));
    assert_eq!(c.source_range(), Some(tr(130, 70)));
}

#[test]
fn trim_past_source_bounds_is_rejected() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 100));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let clip = project
        .add_clip(v1, media_id, tr(90, 10), rt(0))
        .unwrap();

    assert_eq!(
        project.trim_clip(clip, tr(0, 40)),
        Err(ModelError::SourceOutOfBounds)
    );
}

#[test]
fn trim_into_neighbour_is_rejected() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let a = project
        .add_clip(v1, media_id, tr(0, 100), rt(0))
        .unwrap();
    project
        .add_clip(v1, media_id, tr(0, 100), rt(100))
        .unwrap();

    assert_eq!(
        project.trim_clip(a, tr(0, 150)),
        Err(ModelError::Overlap(v1))
    );
}

#[test]
fn move_clip_across_tracks_and_rejects_overlap() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let v2 = project.add_track(TrackKind::Video, "V2");
    let clip = project
        .add_clip(v1, media_id, tr(0, 100), rt(0))
        .unwrap();
    project
        .add_clip(v2, media_id, tr(0, 100), rt(0))
        .unwrap();

    assert_eq!(project.move_clip(clip, v2, rt(0)), Err(ModelError::Overlap(v2)));
    assert_eq!(project.timeline().track_of(clip), Some(v1));

    project.move_clip(clip, v2, rt(200)).unwrap();
    assert_eq!(project.timeline().track_of(clip), Some(v2));
    assert_eq!(project.clip(clip).unwrap().timeline, tr(200, 100));
}

#[test]
fn ripple_delete_closes_the_gap() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let a = project
        .add_clip(v1, media_id, tr(0, 100), rt(0))
        .unwrap();
    let b = project
        .add_clip(v1, media_id, tr(100, 100), rt(100))
        .unwrap();
    let c = project
        .add_clip(v1, media_id, tr(200, 100), rt(200))
        .unwrap();

    project.ripple_delete(b).unwrap();
    assert!(project.clip(b).is_none());
    assert_eq!(project.clip(a).unwrap().start().value, 0);
    assert_eq!(project.clip(c).unwrap().start().value, 100);
    assert_eq!(project.timeline().duration().value, 200);
}

#[test]
fn editing_unknown_clip_errors() {
    let mut project = Project::new("demo", FPS_24);
    let media_id = project.add_media(sample_media(FPS_24, 1000));
    let v1 = project.add_track(TrackKind::Video, "V1");
    let clip = project
        .add_clip(v1, media_id, tr(0, 100), rt(0))
        .unwrap();
    let gone = project.ripple_delete(clip).unwrap().id;

    assert!(matches!(
        project.split_clip(gone, rt(5)),
        Err(ModelError::UnknownClip(_))
    ));
    assert!(matches!(
        project.trim_clip(gone, tr(0, 10)),
        Err(ModelError::UnknownClip(_))
    ));
    assert!(matches!(
        project.move_clip(gone, v1, rt(0)),
        Err(ModelError::UnknownClip(_))
    ));
}
