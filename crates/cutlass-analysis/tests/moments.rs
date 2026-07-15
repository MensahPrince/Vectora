use std::str::FromStr;

use cutlass_analysis::TimeSpan;
use cutlass_analysis::moments::{
    AnalyzerIdentity, AnalyzerIdentityError, AnalyzerVersion, AnalyzerVersionError,
    MAX_ANALYZER_IDENTITY_BYTES, MAX_MOMENT_KIND_BYTES, MAX_MOMENT_PAYLOAD_BYTES,
    MEDIA_CONTENT_KEY_BYTES, MEDIA_CONTENT_KEY_HEX_BYTES, MediaContentKey, MediaContentKeyError,
    MomentBatchError, MomentBatchKey, MomentConfidence, MomentConfidenceError, MomentKind,
    MomentKindError, MomentPayload, MomentPayloadError, MomentQuery, MomentRecord,
    MomentReplacementBatch, filter_and_sort_moments,
};

fn content_key(marker: u8) -> MediaContentKey {
    MediaContentKey::from_bytes([marker; MEDIA_CONTENT_KEY_BYTES])
}

fn analyzer(identity: &str) -> AnalyzerIdentity {
    AnalyzerIdentity::new(identity).expect("test analyzer identity is valid")
}

fn version(value: u32) -> AnalyzerVersion {
    AnalyzerVersion::new(value).expect("test analyzer version is valid")
}

#[allow(clippy::too_many_arguments)]
fn record(
    key: MediaContentKey,
    start: f64,
    end: f64,
    kind: &str,
    confidence: f32,
    analyzer_identity: &str,
    analyzer_version: u32,
    payload: Option<&str>,
) -> MomentRecord {
    MomentRecord::new(
        key,
        TimeSpan::new(start, end).expect("test time span is valid"),
        MomentKind::new(kind).expect("test moment kind is valid"),
        MomentConfidence::new(confidence).expect("test confidence is valid"),
        analyzer(analyzer_identity),
        version(analyzer_version),
        payload.map(|text| MomentPayload::new(text).expect("test payload is valid")),
    )
}

fn payload_label(record: &MomentRecord) -> &str {
    record.payload().map_or("<none>", MomentPayload::as_str)
}

#[test]
fn content_key_binary_and_hex_forms_round_trip_canonically() {
    let bytes = std::array::from_fn(|index| index as u8);
    let key = MediaContentKey::from_slice(&bytes).expect("32 bytes form a key");
    let expected = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    assert_eq!(key.as_bytes(), &bytes);
    assert_eq!(key.into_bytes(), bytes);
    assert_eq!(key.to_string(), expected);
    assert_eq!(key.encode_hex().as_slice(), expected.as_bytes());
    assert_eq!(expected.len(), MEDIA_CONTENT_KEY_HEX_BYTES);
    assert_eq!(MediaContentKey::from_str(expected), Ok(key));
    assert_eq!(
        MediaContentKey::from_str(&expected.to_ascii_uppercase()),
        Ok(key)
    );
}

#[test]
fn content_key_reports_exact_length_and_invalid_hex_errors() {
    assert_eq!(
        MediaContentKey::from_slice(&[0; MEDIA_CONTENT_KEY_BYTES - 1]),
        Err(MediaContentKeyError::InvalidByteLength {
            actual: MEDIA_CONTENT_KEY_BYTES - 1,
        })
    );
    assert_eq!(
        MediaContentKey::from_slice(&[0; MEDIA_CONTENT_KEY_BYTES + 1]),
        Err(MediaContentKeyError::InvalidByteLength {
            actual: MEDIA_CONTENT_KEY_BYTES + 1,
        })
    );
    assert_eq!(
        "0".repeat(MEDIA_CONTENT_KEY_HEX_BYTES - 1)
            .parse::<MediaContentKey>(),
        Err(MediaContentKeyError::InvalidHexLength {
            actual: MEDIA_CONTENT_KEY_HEX_BYTES - 1,
        })
    );
    assert_eq!(
        "0".repeat(MEDIA_CONTENT_KEY_HEX_BYTES + 1)
            .parse::<MediaContentKey>(),
        Err(MediaContentKeyError::InvalidHexLength {
            actual: MEDIA_CONTENT_KEY_HEX_BYTES + 1,
        })
    );

    let mut invalid = "0".repeat(MEDIA_CONTENT_KEY_HEX_BYTES);
    invalid.replace_range(17..18, "g");
    assert_eq!(
        invalid.parse::<MediaContentKey>(),
        Err(MediaContentKeyError::InvalidHexCharacter {
            index: 17,
            byte: b'g',
        })
    );
}

#[test]
fn built_in_and_custom_moment_kinds_are_stable_and_bounded() {
    let built_ins = [
        (MomentKind::SHOT_BOUNDARY, "shot_boundary"),
        (MomentKind::SILENCE, "silence"),
        (MomentKind::BEAT, "beat"),
        (MomentKind::TRANSCRIPT_SEGMENT, "transcript_segment"),
        (MomentKind::TRANSCRIPT_WORD, "transcript_word"),
        (MomentKind::VISION_MOMENT, "vision_moment"),
    ];

    for (kind, expected) in built_ins {
        assert_eq!(kind.as_str(), expected);
        assert!(kind.is_builtin());
        assert_eq!(MomentKind::new(expected), Ok(kind));
    }

    let custom = MomentKind::new("gameplay.kill-confirmed").expect("custom kind is valid");
    assert_eq!(custom.as_str(), "gameplay.kill-confirmed");
    assert!(!custom.is_builtin());

    let maximum = format!("a{}", "x".repeat(MAX_MOMENT_KIND_BYTES - 1));
    assert_eq!(
        MomentKind::new(&maximum)
            .expect("maximum kind length is valid")
            .as_str(),
        maximum
    );
    assert_eq!(MomentKind::new(""), Err(MomentKindError::Empty));
    assert_eq!(
        MomentKind::new(&format!("a{}", "x".repeat(MAX_MOMENT_KIND_BYTES))),
        Err(MomentKindError::TooLong {
            actual: MAX_MOMENT_KIND_BYTES + 1,
        })
    );
    assert_eq!(
        MomentKind::new("Shot"),
        Err(MomentKindError::InvalidStart { byte: b'S' })
    );
    assert_eq!(
        MomentKind::new("custom/kind"),
        Err(MomentKindError::InvalidCharacter {
            index: 6,
            byte: b'/',
        })
    );
}

#[test]
fn analyzer_identities_are_namespaced_bounded_ascii_values() {
    let identity =
        AnalyzerIdentity::new("cutlass/audio/silence-v2").expect("namespaced identity is valid");
    assert_eq!(identity.as_str(), "cutlass/audio/silence-v2");

    let maximum = format!("a{}", "x".repeat(MAX_ANALYZER_IDENTITY_BYTES - 1));
    assert_eq!(
        AnalyzerIdentity::new(&maximum)
            .expect("maximum identity length is valid")
            .as_str(),
        maximum
    );
    assert_eq!(AnalyzerIdentity::new(""), Err(AnalyzerIdentityError::Empty));
    assert_eq!(
        AnalyzerIdentity::new(&format!("a{}", "x".repeat(MAX_ANALYZER_IDENTITY_BYTES))),
        Err(AnalyzerIdentityError::TooLong {
            actual: MAX_ANALYZER_IDENTITY_BYTES + 1,
        })
    );
    assert_eq!(
        AnalyzerIdentity::new("Cutlass"),
        Err(AnalyzerIdentityError::InvalidStart { byte: b'C' })
    );
    assert_eq!(
        AnalyzerIdentity::new("cutlass:shots"),
        Err(AnalyzerIdentityError::InvalidCharacter {
            index: 7,
            byte: b':',
        })
    );
}

#[test]
fn payloads_are_bounded_text_without_opaque_parsing() {
    let text = "speaker: Héllo\tworld\n{\"opaque\":true}";
    let payload = MomentPayload::new(text).expect("ordinary UTF-8 text is valid");
    assert_eq!(payload.as_str(), text);
    assert_eq!(payload.len_bytes(), text.len());

    let maximum = "x".repeat(MAX_MOMENT_PAYLOAD_BYTES);
    assert_eq!(
        MomentPayload::new(&maximum)
            .expect("maximum payload length is valid")
            .len_bytes(),
        MAX_MOMENT_PAYLOAD_BYTES
    );
    assert_eq!(MomentPayload::new(""), Err(MomentPayloadError::Empty));
    assert_eq!(
        MomentPayload::new(&"x".repeat(MAX_MOMENT_PAYLOAD_BYTES + 1)),
        Err(MomentPayloadError::TooLong {
            actual: MAX_MOMENT_PAYLOAD_BYTES + 1,
        })
    );
    assert_eq!(
        MomentPayload::new("ok\0bad"),
        Err(MomentPayloadError::DisallowedControl {
            index: 2,
            character: '\0',
        })
    );
}

#[test]
fn confidence_rejects_nonfinite_and_out_of_range_values() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            MomentConfidence::new(value),
            Err(MomentConfidenceError::NonFinite)
        );
    }
    for value in [-0.001, 1.001] {
        assert_eq!(
            MomentConfidence::new(value),
            Err(MomentConfidenceError::OutOfRange)
        );
    }

    assert_eq!(MomentConfidence::new(0.0), Ok(MomentConfidence::ZERO));
    assert_eq!(MomentConfidence::new(1.0), Ok(MomentConfidence::CERTAIN));
    assert_eq!(
        MomentConfidence::new(-0.0)
            .expect("negative zero is in range")
            .get()
            .to_bits(),
        0.0_f32.to_bits()
    );
    assert_eq!(AnalyzerVersion::new(0), Err(AnalyzerVersionError::Zero));
    assert_eq!(
        AnalyzerVersion::new(u32::MAX).map(AnalyzerVersion::get),
        Ok(u32::MAX)
    );
}

#[test]
fn query_time_boundaries_distinguish_spans_and_point_events() {
    let key = content_key(1);
    let records = vec![
        record(
            key,
            0.0,
            1.0,
            "silence",
            1.0,
            "cutlass/test",
            1,
            Some("span-ending-at-start"),
        ),
        record(
            key,
            0.5,
            1.25,
            "silence",
            1.0,
            "cutlass/test",
            1,
            Some("span-overlapping-start"),
        ),
        record(
            key,
            1.0,
            1.0,
            "beat",
            1.0,
            "cutlass/test",
            1,
            Some("point-at-start"),
        ),
        record(
            key,
            1.0,
            1.4,
            "silence",
            1.0,
            "cutlass/test",
            1,
            Some("span-at-start"),
        ),
        record(
            key,
            1.5,
            1.5,
            "beat",
            1.0,
            "cutlass/test",
            1,
            Some("point-inside"),
        ),
        record(
            key,
            1.75,
            2.0,
            "silence",
            1.0,
            "cutlass/test",
            1,
            Some("span-ending-at-end"),
        ),
        record(
            key,
            2.0,
            2.0,
            "beat",
            1.0,
            "cutlass/test",
            1,
            Some("point-at-end"),
        ),
        record(
            key,
            2.0,
            2.5,
            "silence",
            1.0,
            "cutlass/test",
            1,
            Some("span-at-end"),
        ),
    ];
    let query = MomentQuery::new(key)
        .with_time_range(TimeSpan::new(1.0, 2.0).expect("query range is valid"));

    let labels: Vec<_> = filter_and_sort_moments(&records, &query)
        .into_iter()
        .map(payload_label)
        .collect();
    assert_eq!(
        labels,
        [
            "span-overlapping-start",
            "point-at-start",
            "span-at-start",
            "point-inside",
            "span-ending-at-end",
        ]
    );

    let empty_query = MomentQuery::new(key)
        .with_time_range(TimeSpan::new(1.0, 1.0).expect("empty query range is valid"));
    assert!(filter_and_sort_moments(&records, &empty_query).is_empty());
}

#[test]
fn query_isolates_content_and_applies_exact_kind_and_inclusive_confidence() {
    let target = content_key(1);
    let other = content_key(2);
    let records = vec![
        record(
            target,
            0.0,
            0.0,
            "beat",
            0.5,
            "cutlass/beats",
            1,
            Some("inclusive-threshold"),
        ),
        record(
            target,
            1.0,
            1.0,
            "beat",
            0.49,
            "cutlass/beats",
            1,
            Some("below-threshold"),
        ),
        record(
            target,
            2.0,
            3.0,
            "silence",
            0.9,
            "cutlass/silence",
            1,
            Some("wrong-kind"),
        ),
        record(
            other,
            0.0,
            0.0,
            "beat",
            1.0,
            "cutlass/beats",
            1,
            Some("wrong-content"),
        ),
    ];
    let query = MomentQuery::new(target)
        .with_kind(MomentKind::BEAT)
        .with_minimum_confidence(MomentConfidence::new(0.5).expect("threshold is valid"));

    let matches = filter_and_sort_moments(&records, &query);
    assert_eq!(matches.len(), 1);
    assert_eq!(payload_label(matches[0]), "inclusive-threshold");
}

#[test]
fn canonical_order_is_independent_of_input_order_and_uses_all_tie_breakers() {
    let key = content_key(3);
    let records = vec![
        record(key, 1.0, 2.0, "silence", 0.5, "source/a", 2, Some("b")),
        record(key, 2.0, 3.0, "beat", 0.5, "source/a", 1, Some("late")),
        record(
            key,
            1.0,
            2.0,
            "silence",
            0.5,
            "source/b",
            1,
            Some("source-b"),
        ),
        record(
            key,
            0.0,
            1.0,
            "beat",
            0.5,
            "source/a",
            1,
            Some("early-span"),
        ),
        record(key, 1.0, 2.0, "silence", 0.5, "source/a", 2, None),
        record(key, 1.0, 2.0, "beat", 0.5, "source/z", 1, Some("kind-beat")),
        record(
            key,
            1.0,
            2.0,
            "silence",
            0.9,
            "source/a",
            2,
            Some("high-confidence"),
        ),
        record(
            key,
            0.0,
            0.0,
            "vision_moment",
            0.5,
            "source/a",
            1,
            Some("early-point"),
        ),
        record(
            key,
            1.0,
            2.0,
            "silence",
            0.5,
            "source/a",
            1,
            Some("version-1"),
        ),
        record(key, 1.0, 2.0, "silence", 0.5, "source/a", 2, Some("a")),
    ];
    let query = MomentQuery::new(key);
    let expected = [
        "early-point",
        "early-span",
        "kind-beat",
        "version-1",
        "high-confidence",
        "<none>",
        "a",
        "b",
        "source-b",
        "late",
    ];

    let labels: Vec<_> = filter_and_sort_moments(&records, &query)
        .into_iter()
        .map(payload_label)
        .collect();
    assert_eq!(labels, expected);

    for shift in 0..records.len() {
        let mut shuffled = records.clone();
        shuffled.rotate_left(shift);
        if shift % 2 == 0 {
            shuffled.reverse();
        }
        let repeated: Vec<_> = filter_and_sort_moments(&shuffled, &query)
            .into_iter()
            .map(payload_label)
            .collect();
        assert_eq!(repeated, expected, "ordering changed for shuffle {shift}");
    }
}

#[test]
fn replacement_batch_rejects_each_scope_mismatch() {
    let key = content_key(4);
    let source = analyzer("cutlass/shots");
    let source_version = version(2);
    let batch_key = MomentBatchKey::new(key, source.clone(), source_version);
    let valid = record(
        key,
        0.0,
        0.0,
        "shot_boundary",
        1.0,
        source.as_str(),
        source_version.get(),
        None,
    );

    let wrong_content = record(
        content_key(5),
        1.0,
        1.0,
        "shot_boundary",
        1.0,
        source.as_str(),
        source_version.get(),
        None,
    );
    assert_eq!(
        MomentReplacementBatch::new(batch_key.clone(), vec![valid.clone(), wrong_content]),
        Err(MomentBatchError::ContentKeyMismatch { record_index: 1 })
    );

    let wrong_analyzer = record(
        key,
        1.0,
        1.0,
        "shot_boundary",
        1.0,
        "other/shots",
        source_version.get(),
        None,
    );
    assert_eq!(
        MomentReplacementBatch::new(batch_key.clone(), vec![valid.clone(), wrong_analyzer]),
        Err(MomentBatchError::AnalyzerIdentityMismatch { record_index: 1 })
    );

    let wrong_version = record(
        key,
        1.0,
        1.0,
        "shot_boundary",
        1.0,
        source.as_str(),
        3,
        None,
    );
    assert_eq!(
        MomentReplacementBatch::new(batch_key, vec![valid, wrong_version]),
        Err(MomentBatchError::AnalyzerVersionMismatch { record_index: 1 })
    );
}

#[test]
fn replacement_batch_rejects_exact_duplicates_and_canonicalizes_records() {
    let key = content_key(6);
    let batch_key = MomentBatchKey::new(key, analyzer("cutlass/vision"), version(1));
    let first = record(
        key,
        2.0,
        3.0,
        "vision_moment",
        0.8,
        "cutlass/vision",
        1,
        Some("second"),
    );
    let second = record(
        key,
        1.0,
        2.0,
        "vision_moment",
        0.8,
        "cutlass/vision",
        1,
        Some("first"),
    );

    assert_eq!(
        MomentReplacementBatch::new(
            batch_key.clone(),
            vec![first.clone(), second.clone(), first.clone()],
        ),
        Err(MomentBatchError::DuplicateRecord {
            first_index: 0,
            duplicate_index: 2,
        })
    );

    let same_time_different_payload = record(
        key,
        2.0,
        3.0,
        "vision_moment",
        0.8,
        "cutlass/vision",
        1,
        Some("third"),
    );
    let batch = MomentReplacementBatch::new(
        batch_key.clone(),
        vec![first, same_time_different_payload, second],
    )
    .expect("distinct matching records form a batch");
    assert_eq!(batch.key(), &batch_key);
    assert_eq!(batch.len(), 3);
    let labels: Vec<_> = batch.records().iter().map(payload_label).collect();
    assert_eq!(labels, ["first", "second", "third"]);

    let (returned_key, returned_records) = batch.into_parts();
    assert_eq!(returned_key, batch_key);
    assert_eq!(returned_records.len(), 3);
}

#[test]
fn empty_queries_and_empty_replacement_batches_are_valid_and_deterministic() {
    let key = content_key(7);
    let query = MomentQuery::new(key);
    assert!(filter_and_sort_moments(&[], &query).is_empty());
    assert!(filter_and_sort_moments(&[], &query).is_empty());

    let batch_key = MomentBatchKey::new(key, analyzer("cutlass/empty"), version(1));
    let batch = MomentReplacementBatch::new(batch_key.clone(), Vec::new())
        .expect("empty complete result sets are valid");
    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
    assert_eq!(batch.key(), &batch_key);
    assert!(batch.records().is_empty());
}
