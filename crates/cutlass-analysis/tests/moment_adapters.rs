use std::panic;

use cutlass_analysis::moments::{
    AdapterRecordKind, AnalyzerIdentity, AnalyzerVersion, MEDIA_CONTENT_KEY_BYTES, MediaContentKey,
    MomentBatchKey, MomentConfidence, MomentKind, MomentRecord, MomentReplacementBatch,
    beat_events_to_moment_batch, shot_boundaries_to_moment_batch, silence_spans_to_moment_batch,
};
use cutlass_analysis::{
    BeatConfig, BeatEvent, LoudnessConfig, Rgba8Frame, ShotBoundaryKind, ShotDetectionConfig,
    ShotScoreWeights, SilenceConfig, SilenceSpan, TimeSpan, detect_beats, detect_shots,
    detect_silence,
};

fn content_key(marker: u8) -> MediaContentKey {
    MediaContentKey::from_bytes([marker; MEDIA_CONTENT_KEY_BYTES])
}

fn analyzer(value: &str) -> AnalyzerIdentity {
    AnalyzerIdentity::new(value).expect("test analyzer identity is valid")
}

fn version(value: u32) -> AnalyzerVersion {
    AnalyzerVersion::new(value).expect("test analyzer version is valid")
}

fn payload_bytes(record: &MomentRecord) -> &[u8] {
    record
        .payload()
        .expect("adapter records always carry payloads")
        .as_str()
        .as_bytes()
}

fn assert_scope(
    batch: &MomentReplacementBatch,
    key: MediaContentKey,
    identity: &AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
) {
    assert_eq!(
        batch.key(),
        &MomentBatchKey::new(key, identity.clone(), analyzer_version)
    );
    assert!(batch.records().iter().all(|record| {
        record.content_key() == key
            && record.analyzer_identity() == identity
            && record.analyzer_version() == analyzer_version
    }));
}

fn checkerboard_rgba(width: usize, height: usize, inverted: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let bright = ((x + y) % 2 == 0) ^ inverted;
            let value = if bright { 255 } else { 0 };
            bytes.extend_from_slice(&[value, value, value, 255]);
        }
    }
    bytes
}

#[test]
fn detected_audio_outputs_convert_to_exact_batches() {
    let loudness = LoudnessConfig::new(0.01, 0.01, -100.0).expect("test loudness config is valid");

    let mut beat_samples = vec![0.0; 100];
    beat_samples.extend(std::iter::repeat_n(1.0, 10));
    beat_samples.extend(std::iter::repeat_n(0.0, 90));
    let beat_config =
        BeatConfig::new(loudness, 0.05, 0.0, 3.0, 0.0).expect("test beat config is valid");
    let beats = detect_beats(&beat_samples, 1_000, 1, beat_config).expect("synthetic PCM is valid");
    assert_eq!(
        beats,
        [BeatEvent {
            time_seconds: 0.11,
            strength_db: 100.0,
        }]
    );

    let key = content_key(1);
    let beat_identity = analyzer("cutlass/beat-adapter-test");
    let beat_version = version(7);
    let beat_batch = beat_events_to_moment_batch(&beats, key, beat_identity.clone(), beat_version)
        .expect("detected beats adapt");
    let repeated_beat_batch =
        beat_events_to_moment_batch(&beats, key, beat_identity.clone(), beat_version)
            .expect("repeated detected beats adapt");
    assert_eq!(beat_batch, repeated_beat_batch);
    assert_scope(&beat_batch, key, &beat_identity, beat_version);
    assert_eq!(beat_batch.len(), 1);

    let beat = &beat_batch.records()[0];
    assert_eq!(beat.kind(), &MomentKind::BEAT);
    assert_eq!(
        beat.span(),
        TimeSpan::new(0.11, 0.11).expect("expected point span is valid")
    );
    assert_eq!(beat.confidence(), MomentConfidence::CERTAIN);
    assert_eq!(
        payload_bytes(beat),
        br#"{"v":1,"index":0,"strength_db":100}"#
    );

    let mut silence_samples = vec![1.0; 100];
    silence_samples.extend(std::iter::repeat_n(0.0, 200));
    silence_samples.extend(std::iter::repeat_n(1.0, 100));
    let silence_config =
        SilenceConfig::new(loudness, -40.0, 0.1, 0.0, 0.0).expect("test silence config is valid");
    let silences =
        detect_silence(&silence_samples, 1_000, 1, silence_config).expect("synthetic PCM is valid");
    assert_eq!(
        silences,
        [SilenceSpan {
            span: TimeSpan::new(0.1, 0.3).expect("expected silence span is valid"),
        }]
    );

    let silence_identity = analyzer("cutlass/silence-adapter-test");
    let silence_version = version(3);
    let silence_batch =
        silence_spans_to_moment_batch(&silences, key, silence_identity.clone(), silence_version)
            .expect("detected silences adapt");
    let repeated_silence_batch =
        silence_spans_to_moment_batch(&silences, key, silence_identity.clone(), silence_version)
            .expect("repeated detected silences adapt");
    assert_eq!(silence_batch, repeated_silence_batch);
    assert_scope(&silence_batch, key, &silence_identity, silence_version);
    assert_eq!(silence_batch.len(), 1);

    let silence = &silence_batch.records()[0];
    assert_eq!(silence.kind(), &MomentKind::SILENCE);
    assert_eq!(
        silence.span(),
        TimeSpan::new(0.1, 0.3).expect("expected silence span is valid")
    );
    assert_eq!(silence.confidence(), MomentConfidence::CERTAIN);
    assert_eq!(payload_bytes(silence), br#"{"v":1,"index":0}"#);
}

#[test]
fn detected_shots_convert_with_exact_point_semantics_and_payloads() {
    let first = checkerboard_rgba(8, 8, false);
    let second = checkerboard_rgba(8, 8, true);
    let dimension_changed = checkerboard_rgba(9, 8, false);
    let frames = [
        Rgba8Frame::new(&first, 8, 8, 32, 0.0).expect("first frame is valid"),
        Rgba8Frame::new(&second, 8, 8, 32, 1.0).expect("second frame is valid"),
        Rgba8Frame::new(&dimension_changed, 9, 8, 36, 2.0)
            .expect("dimension-changed frame is valid"),
    ];
    let config = ShotDetectionConfig::new(8, 8, 16, ShotScoreWeights::default(), 0.30, 0.0)
        .expect("test shot config is valid");
    let boundaries = detect_shots(&frames, config).expect("synthetic frames are ordered");
    assert_eq!(boundaries.len(), 2);
    assert_eq!(boundaries[0].kind(), ShotBoundaryKind::VisualChange);
    assert_eq!(boundaries[1].kind(), ShotBoundaryKind::DimensionChange);

    let key = content_key(2);
    let identity = analyzer("cutlass/shot-adapter-test");
    let analyzer_version = version(11);
    let batch =
        shot_boundaries_to_moment_batch(&boundaries, key, identity.clone(), analyzer_version)
            .expect("detected shot boundaries adapt");
    let repeated =
        shot_boundaries_to_moment_batch(&boundaries, key, identity.clone(), analyzer_version)
            .expect("repeated detected shot boundaries adapt");
    assert_eq!(batch, repeated);
    assert_scope(&batch, key, &identity, analyzer_version);
    assert_eq!(batch.len(), 2);

    let visual = &batch.records()[0];
    assert_eq!(visual.kind(), &MomentKind::SHOT_BOUNDARY);
    assert_eq!(
        visual.span(),
        TimeSpan::new(1.0, 1.0).expect("expected shot point is valid")
    );
    assert_eq!(
        visual.confidence(),
        MomentConfidence::new(boundaries[0].score()).expect("detector score is normalized")
    );
    assert_eq!(
        payload_bytes(visual),
        br#"{"v":1,"index":0,"kind":"visual_change","previous_seconds":0,"current_seconds":1}"#
    );

    let dimension = &batch.records()[1];
    assert_eq!(dimension.kind(), &MomentKind::SHOT_BOUNDARY);
    assert_eq!(
        dimension.span(),
        TimeSpan::new(2.0, 2.0).expect("expected shot point is valid")
    );
    assert_eq!(dimension.confidence(), MomentConfidence::CERTAIN);
    assert_eq!(
        payload_bytes(dimension),
        br#"{"v":1,"index":1,"kind":"dimension_change","previous_seconds":1,"current_seconds":2}"#
    );
}

#[test]
fn empty_inputs_produce_valid_empty_replacement_scopes() {
    let key = content_key(3);
    let identity = analyzer("cutlass/empty-adapter-test");
    let analyzer_version = version(5);

    let batches = [
        beat_events_to_moment_batch(&[], key, identity.clone(), analyzer_version)
            .expect("empty beats adapt"),
        silence_spans_to_moment_batch(&[], key, identity.clone(), analyzer_version)
            .expect("empty silences adapt"),
        shot_boundaries_to_moment_batch(&[], key, identity.clone(), analyzer_version)
            .expect("empty shots adapt"),
    ];

    for batch in &batches {
        assert_scope(batch, key, &identity, analyzer_version);
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
        assert!(batch.records().is_empty());
    }
}

#[test]
fn source_ordinals_distinguish_duplicate_beats_and_silences() {
    let key = content_key(4);
    let analyzer_version = version(1);
    let beats = [
        BeatEvent {
            time_seconds: 2.5,
            strength_db: 6.25,
        },
        BeatEvent {
            time_seconds: 2.5,
            strength_db: 6.25,
        },
    ];
    let beat_batch = beat_events_to_moment_batch(
        &beats,
        key,
        analyzer("cutlass/duplicate-beats-test"),
        analyzer_version,
    )
    .expect("same-time beats remain distinct");
    assert_eq!(beat_batch.len(), 2);
    assert_eq!(
        payload_bytes(&beat_batch.records()[0]),
        br#"{"v":1,"index":0,"strength_db":6.25}"#
    );
    assert_eq!(
        payload_bytes(&beat_batch.records()[1]),
        br#"{"v":1,"index":1,"strength_db":6.25}"#
    );

    let duplicate_span = SilenceSpan {
        span: TimeSpan::new(3.0, 4.0).expect("duplicate test span is valid"),
    };
    let silence_batch = silence_spans_to_moment_batch(
        &[duplicate_span, duplicate_span],
        key,
        analyzer("cutlass/duplicate-silences-test"),
        analyzer_version,
    )
    .expect("duplicate silence spans remain distinct");
    assert_eq!(silence_batch.len(), 2);
    assert_eq!(
        payload_bytes(&silence_batch.records()[0]),
        br#"{"v":1,"index":0}"#
    );
    assert_eq!(
        payload_bytes(&silence_batch.records()[1]),
        br#"{"v":1,"index":1}"#
    );
}

#[test]
fn invalid_public_beat_values_return_bounded_typed_errors_without_panicking() {
    let invalid_cases = [
        (
            BeatEvent {
                time_seconds: f64::NAN,
                strength_db: 1.0,
            },
            "time_seconds",
            "must be finite",
        ),
        (
            BeatEvent {
                time_seconds: f64::INFINITY,
                strength_db: 1.0,
            },
            "time_seconds",
            "must be finite",
        ),
        (
            BeatEvent {
                time_seconds: -0.25,
                strength_db: 1.0,
            },
            "time_seconds",
            "must be non-negative",
        ),
        (
            BeatEvent {
                time_seconds: 1.0,
                strength_db: f32::NAN,
            },
            "strength_db",
            "must be finite",
        ),
        (
            BeatEvent {
                time_seconds: 1.0,
                strength_db: f32::INFINITY,
            },
            "strength_db",
            "must be finite",
        ),
        (
            BeatEvent {
                time_seconds: 1.0,
                strength_db: -1.0,
            },
            "strength_db",
            "must be positive",
        ),
        (
            BeatEvent {
                time_seconds: 1.0,
                strength_db: 0.0,
            },
            "strength_db",
            "must be positive",
        ),
    ];

    for (invalid, expected_field, expected_reason) in invalid_cases {
        let events = [
            BeatEvent {
                time_seconds: 0.5,
                strength_db: 2.0,
            },
            invalid,
        ];
        let result = panic::catch_unwind(|| {
            beat_events_to_moment_batch(
                &events,
                content_key(5),
                analyzer("cutlass/invalid-beat-test"),
                version(1),
            )
        })
        .expect("invalid externally constructed beats must not panic");
        let error = result.expect_err("invalid externally constructed beat must fail");

        assert_eq!(error.record_kind(), AdapterRecordKind::Beat);
        assert_eq!(error.record_index(), 1);
        assert_eq!(error.field(), expected_field);
        assert_eq!(error.reason(), expected_reason);
        assert!(error.to_string().len() < 160);
    }
}
