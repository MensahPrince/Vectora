//! Conversion from normalized transcripts to analyzer-scoped media moments.

use std::error::Error;
use std::fmt;

use cutlass_analysis::TimeSpan;
use cutlass_analysis::moments::{
    AnalyzerIdentity, AnalyzerVersion, MAX_MOMENT_PAYLOAD_BYTES, MediaContentKey, MomentBatchError,
    MomentBatchKey, MomentConfidence, MomentKind, MomentPayload, MomentPayloadError, MomentRecord,
    MomentReplacementBatch,
};

use crate::{Transcript, TranscriptSegment};

/// Schema version embedded in every transcript moment payload.
///
/// Version 1 is canonical compact JSON with fields in this exact order:
///
/// - segment: `{"v":1,"segment_index":N,"text":"..."}`
/// - word: `{"v":1,"segment_index":N,"word_index":M,"text":"..."}`
///
/// Indices are unpadded decimal integers. Quotes, backslashes, and Unicode
/// control characters are escaped; all other Unicode is emitted directly as
/// UTF-8. There is no insignificant whitespace. The segment and word indices
/// preserve source ordinals and therefore distinguish otherwise identical
/// records.
pub const TRANSCRIPT_MOMENT_PAYLOAD_VERSION: u32 = 1;

/// Confidence assigned to a transcript segment that has no word probabilities.
///
/// Zero is intentionally conservative: segment-level confidence is not
/// supplied by the normalized transcript, so the adapter does not imply model
/// certainty when token timestamps or probabilities are unavailable.
pub const TRANSCRIPT_SEGMENT_FALLBACK_CONFIDENCE: MomentConfidence = MomentConfidence::ZERO;

// Below this inclusive limit, conversion to seconds retains enough f64
// resolution to distinguish every adjacent centisecond. The limit is far
// beyond practical media durations while making adversarial timestamps fail
// closed instead of silently collapsing boundaries.
const MAX_EXACT_F64_CENTISECONDS: u64 = 1_u64 << 52;

/// Stable source ordinal for a transcript moment record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptMomentRecordIndex {
    /// A segment record.
    Segment {
        /// Zero-based index in [`Transcript::segments`].
        segment_index: usize,
    },
    /// A word record.
    Word {
        /// Zero-based index in [`Transcript::segments`].
        segment_index: usize,
        /// Zero-based index in [`crate::TranscriptSegment::words`].
        word_index: usize,
    },
}

impl fmt::Display for TranscriptMomentRecordIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Segment { segment_index } => write!(formatter, "segment[{segment_index}]"),
            Self::Word {
                segment_index,
                word_index,
            } => write!(formatter, "segment[{segment_index}].word[{word_index}]"),
        }
    }
}

/// Stable field category reported by [`TranscriptMomentConversionError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptMomentErrorField {
    /// A record's start timestamp in centiseconds.
    StartCentiseconds,
    /// A record's end timestamp in centiseconds.
    EndCentiseconds,
    /// A converted half-open time span.
    Span,
    /// A segment or word confidence.
    Confidence,
    /// A versioned record payload.
    Payload,
    /// Final atomic replacement-batch validation.
    Batch,
}

impl fmt::Display for TranscriptMomentErrorField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StartCentiseconds => "start_centiseconds",
            Self::EndCentiseconds => "end_centiseconds",
            Self::Span => "span",
            Self::Confidence => "confidence",
            Self::Payload => "payload",
            Self::Batch => "batch",
        })
    }
}

/// Stable failure reason reported by [`TranscriptMomentConversionError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptMomentErrorReason {
    /// A centisecond boundary could not retain centisecond resolution in
    /// seconds-based `f64` storage.
    CentisecondsNotRepresentable,
    /// Converted boundaries did not form a valid moments time span.
    InvalidSpan,
    /// A normalized probability could not form a moments confidence.
    InvalidConfidence,
    /// Canonical JSON would exceed
    /// [`cutlass_analysis::moments::MAX_MOMENT_PAYLOAD_BYTES`].
    PayloadExceedsLimit,
    /// Canonical JSON unexpectedly failed moments payload validation.
    InvalidPayload,
    /// Final validation found a record with another media content key.
    BatchContentKeyMismatch,
    /// Final validation found a record with another analyzer identity.
    BatchAnalyzerIdentityMismatch,
    /// Final validation found a record with another analyzer version.
    BatchAnalyzerVersionMismatch,
    /// Final validation found two exactly equal records.
    BatchDuplicateRecord,
}

impl fmt::Display for TranscriptMomentErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CentisecondsNotRepresentable => {
                "centisecond boundary is not exactly representable at centisecond resolution"
            }
            Self::InvalidSpan => "converted boundaries do not form a valid time span",
            Self::InvalidConfidence => "probability does not form a valid confidence",
            Self::PayloadExceedsLimit => "canonical payload exceeds the maximum encoded size",
            Self::InvalidPayload => "canonical payload failed domain validation",
            Self::BatchContentKeyMismatch => {
                "record content key does not match the replacement scope"
            }
            Self::BatchAnalyzerIdentityMismatch => {
                "record analyzer identity does not match the replacement scope"
            }
            Self::BatchAnalyzerVersionMismatch => {
                "record analyzer version does not match the replacement scope"
            }
            Self::BatchDuplicateRecord => "record duplicates another record in the batch",
        })
    }
}

/// Bounded, non-text-leaking failure to convert a transcript into moments.
///
/// Record failures carry stable transcript ordinals. Batch failures carry the
/// ordinal of the rejected input record when final validation supplied one.
/// Display output contains only that ordinal, a typed field, and a stable
/// reason; transcript text is never included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptMomentConversionError {
    record_index: Option<TranscriptMomentRecordIndex>,
    field: TranscriptMomentErrorField,
    reason: TranscriptMomentErrorReason,
}

impl TranscriptMomentConversionError {
    /// Returns the stable source ordinal, or `None` for a batch-wide failure.
    #[must_use]
    pub const fn record_index(&self) -> Option<TranscriptMomentRecordIndex> {
        self.record_index
    }

    /// Returns the field category that failed conversion or validation.
    #[must_use]
    pub const fn field(&self) -> TranscriptMomentErrorField {
        self.field
    }

    /// Returns the stable, typed failure reason.
    #[must_use]
    pub const fn reason(&self) -> TranscriptMomentErrorReason {
        self.reason
    }

    const fn record(
        record_index: TranscriptMomentRecordIndex,
        field: TranscriptMomentErrorField,
        reason: TranscriptMomentErrorReason,
    ) -> Self {
        Self {
            record_index: Some(record_index),
            field,
            reason,
        }
    }

    fn batch(
        record_index: Option<TranscriptMomentRecordIndex>,
        reason: TranscriptMomentErrorReason,
    ) -> Self {
        Self {
            record_index,
            field: TranscriptMomentErrorField::Batch,
            reason,
        }
    }
}

impl fmt::Display for TranscriptMomentConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(record_index) = self.record_index {
            write!(
                formatter,
                "transcript moment {record_index} field `{}`: {}",
                self.field, self.reason
            )
        } else {
            write!(
                formatter,
                "transcript moment batch field `{}`: {}",
                self.field, self.reason
            )
        }
    }
}

impl Error for TranscriptMomentConversionError {}

/// Converts one normalized transcript into an atomic moments replacement batch.
///
/// The caller supplies the validated content key, analyzer identity, and
/// analyzer version so model and configuration provenance remains an
/// application decision. The transcript is borrowed.
///
/// One `TRANSCRIPT_SEGMENT` record is emitted for every non-empty segment and
/// one `TRANSCRIPT_WORD` record for every word. Centisecond boundaries are
/// converted directly to seconds after verifying that `f64` retains
/// centisecond resolution. Word confidence is the normalized Whisper
/// probability. Segment confidence is the duration-independent arithmetic mean
/// of its word probabilities, computed in `f64`; a segment without words uses
/// [`TRANSCRIPT_SEGMENT_FALLBACK_CONFIDENCE`].
///
/// Payloads use the canonical representation documented by
/// [`TRANSCRIPT_MOMENT_PAYLOAD_VERSION`]. Text is never truncated. An empty
/// transcript returns a valid empty batch, which is an atomic marker that prior
/// results in the same replacement scope should be removed.
///
/// # Errors
///
/// Returns [`TranscriptMomentConversionError`] if a timestamp cannot retain
/// centisecond resolution, confidence or span domain validation fails, an
/// encoded payload would exceed
/// [`cutlass_analysis::moments::MAX_MOMENT_PAYLOAD_BYTES`], or final replacement
/// batch validation fails.
pub fn transcript_to_moment_batch(
    content_key: MediaContentKey,
    analyzer_identity: AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
    transcript: &Transcript,
) -> Result<MomentReplacementBatch, TranscriptMomentConversionError> {
    let mut records = Vec::new();
    let mut record_indices = Vec::new();

    for (segment_index, segment) in transcript.segments().iter().enumerate() {
        if !segment.text().is_empty() {
            let record_index = TranscriptMomentRecordIndex::Segment { segment_index };
            records.push(segment_record(
                content_key,
                &analyzer_identity,
                analyzer_version,
                segment,
                record_index,
            )?);
            record_indices.push(record_index);
        }

        for (word_index, word) in segment.words().iter().enumerate() {
            let record_index = TranscriptMomentRecordIndex::Word {
                segment_index,
                word_index,
            };
            let span = convert_span(
                record_index,
                word.start_centiseconds(),
                word.end_centiseconds(),
            )?;
            let confidence = MomentConfidence::new(word.probability()).map_err(|_| {
                TranscriptMomentConversionError::record(
                    record_index,
                    TranscriptMomentErrorField::Confidence,
                    TranscriptMomentErrorReason::InvalidConfidence,
                )
            })?;
            let payload = encode_payload(record_index, word.text())?;

            records.push(MomentRecord::new(
                content_key,
                span,
                MomentKind::TRANSCRIPT_WORD,
                confidence,
                analyzer_identity.clone(),
                analyzer_version,
                Some(payload),
            ));
            record_indices.push(record_index);
        }
    }

    let key = MomentBatchKey::new(content_key, analyzer_identity, analyzer_version);
    MomentReplacementBatch::new(key, records)
        .map_err(|error| convert_batch_error(error, &record_indices))
}

fn segment_record(
    content_key: MediaContentKey,
    analyzer_identity: &AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
    segment: &TranscriptSegment,
    record_index: TranscriptMomentRecordIndex,
) -> Result<MomentRecord, TranscriptMomentConversionError> {
    let span = convert_span(
        record_index,
        segment.start_centiseconds(),
        segment.end_centiseconds(),
    )?;
    let confidence = segment_confidence(segment).map_err(|_| {
        TranscriptMomentConversionError::record(
            record_index,
            TranscriptMomentErrorField::Confidence,
            TranscriptMomentErrorReason::InvalidConfidence,
        )
    })?;
    let payload = encode_payload(record_index, segment.text())?;

    Ok(MomentRecord::new(
        content_key,
        span,
        MomentKind::TRANSCRIPT_SEGMENT,
        confidence,
        analyzer_identity.clone(),
        analyzer_version,
        Some(payload),
    ))
}

fn segment_confidence(
    segment: &TranscriptSegment,
) -> Result<MomentConfidence, cutlass_analysis::moments::MomentConfidenceError> {
    if segment.words().is_empty() {
        return Ok(TRANSCRIPT_SEGMENT_FALLBACK_CONFIDENCE);
    }

    let probability_sum: f64 = segment
        .words()
        .iter()
        .map(|word| f64::from(word.probability()))
        .sum();
    let mean = probability_sum / segment.words().len() as f64;
    MomentConfidence::new(mean as f32)
}

fn convert_span(
    record_index: TranscriptMomentRecordIndex,
    start_centiseconds: u64,
    end_centiseconds: u64,
) -> Result<TimeSpan, TranscriptMomentConversionError> {
    let start_seconds = convert_boundary(
        record_index,
        TranscriptMomentErrorField::StartCentiseconds,
        start_centiseconds,
    )?;
    let end_seconds = convert_boundary(
        record_index,
        TranscriptMomentErrorField::EndCentiseconds,
        end_centiseconds,
    )?;

    TimeSpan::new(start_seconds, end_seconds).map_err(|_| {
        TranscriptMomentConversionError::record(
            record_index,
            TranscriptMomentErrorField::Span,
            TranscriptMomentErrorReason::InvalidSpan,
        )
    })
}

fn convert_boundary(
    record_index: TranscriptMomentRecordIndex,
    field: TranscriptMomentErrorField,
    centiseconds: u64,
) -> Result<f64, TranscriptMomentConversionError> {
    if centiseconds > MAX_EXACT_F64_CENTISECONDS {
        return Err(TranscriptMomentConversionError::record(
            record_index,
            field,
            TranscriptMomentErrorReason::CentisecondsNotRepresentable,
        ));
    }

    let seconds = centiseconds as f64 / 100.0;
    let recovered_centiseconds = (seconds * 100.0).round();
    if !seconds.is_finite() || recovered_centiseconds != centiseconds as f64 {
        return Err(TranscriptMomentConversionError::record(
            record_index,
            field,
            TranscriptMomentErrorReason::CentisecondsNotRepresentable,
        ));
    }

    Ok(seconds)
}

fn encode_payload(
    record_index: TranscriptMomentRecordIndex,
    text: &str,
) -> Result<MomentPayload, TranscriptMomentConversionError> {
    let initial_capacity = text.len().saturating_add(80).min(MAX_MOMENT_PAYLOAD_BYTES);
    let mut payload = String::with_capacity(initial_capacity);

    push_payload(&mut payload, "{\"v\":1,\"segment_index\":", record_index)?;
    let segment_index = match record_index {
        TranscriptMomentRecordIndex::Segment { segment_index }
        | TranscriptMomentRecordIndex::Word { segment_index, .. } => segment_index,
    };
    push_payload(&mut payload, &segment_index.to_string(), record_index)?;

    if let TranscriptMomentRecordIndex::Word { word_index, .. } = record_index {
        push_payload(&mut payload, ",\"word_index\":", record_index)?;
        push_payload(&mut payload, &word_index.to_string(), record_index)?;
    }

    push_payload(&mut payload, ",\"text\":\"", record_index)?;
    push_json_string_contents(&mut payload, text, record_index)?;
    push_payload(&mut payload, "\"}", record_index)?;

    MomentPayload::new(&payload).map_err(|error| {
        let reason = match error {
            MomentPayloadError::TooLong { .. } => TranscriptMomentErrorReason::PayloadExceedsLimit,
            MomentPayloadError::Empty | MomentPayloadError::DisallowedControl { .. } => {
                TranscriptMomentErrorReason::InvalidPayload
            }
        };
        TranscriptMomentConversionError::record(
            record_index,
            TranscriptMomentErrorField::Payload,
            reason,
        )
    })
}

fn push_json_string_contents(
    output: &mut String,
    text: &str,
    record_index: TranscriptMomentRecordIndex,
) -> Result<(), TranscriptMomentConversionError> {
    for character in text.chars() {
        match character {
            '"' => push_payload(output, "\\\"", record_index)?,
            '\\' => push_payload(output, "\\\\", record_index)?,
            character if character.is_control() => {
                push_json_unicode_escape(output, character, record_index)?;
            }
            character => push_payload_character(output, character, record_index)?,
        }
    }
    Ok(())
}

fn push_json_unicode_escape(
    output: &mut String,
    character: char,
    record_index: TranscriptMomentRecordIndex,
) -> Result<(), TranscriptMomentConversionError> {
    let scalar = u32::from(character);
    if let Ok(code_unit) = u16::try_from(scalar) {
        push_json_code_unit(output, code_unit, record_index)
    } else {
        let supplementary = scalar - 0x1_0000;
        let high = 0xd800 | u16::try_from(supplementary >> 10).unwrap_or(0);
        let low = 0xdc00 | u16::try_from(supplementary & 0x03ff).unwrap_or(0);
        push_json_code_unit(output, high, record_index)?;
        push_json_code_unit(output, low, record_index)
    }
}

fn push_json_code_unit(
    output: &mut String,
    code_unit: u16,
    record_index: TranscriptMomentRecordIndex,
) -> Result<(), TranscriptMomentConversionError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    push_payload(output, "\\u", record_index)?;
    for shift in [12, 8, 4, 0] {
        let nibble = usize::from((code_unit >> shift) & 0x000f);
        push_payload_character(output, char::from(HEX[nibble]), record_index)?;
    }
    Ok(())
}

fn push_payload(
    output: &mut String,
    value: &str,
    record_index: TranscriptMomentRecordIndex,
) -> Result<(), TranscriptMomentConversionError> {
    let encoded_len = output.len().checked_add(value.len());
    if encoded_len.is_none_or(|length| length > MAX_MOMENT_PAYLOAD_BYTES) {
        return Err(payload_too_large(record_index));
    }
    output.push_str(value);
    Ok(())
}

fn push_payload_character(
    output: &mut String,
    character: char,
    record_index: TranscriptMomentRecordIndex,
) -> Result<(), TranscriptMomentConversionError> {
    let encoded_len = output.len().checked_add(character.len_utf8());
    if encoded_len.is_none_or(|length| length > MAX_MOMENT_PAYLOAD_BYTES) {
        return Err(payload_too_large(record_index));
    }
    output.push(character);
    Ok(())
}

const fn payload_too_large(
    record_index: TranscriptMomentRecordIndex,
) -> TranscriptMomentConversionError {
    TranscriptMomentConversionError::record(
        record_index,
        TranscriptMomentErrorField::Payload,
        TranscriptMomentErrorReason::PayloadExceedsLimit,
    )
}

fn convert_batch_error(
    error: MomentBatchError,
    record_indices: &[TranscriptMomentRecordIndex],
) -> TranscriptMomentConversionError {
    let (record_position, reason) = match error {
        MomentBatchError::ContentKeyMismatch { record_index } => (
            record_index,
            TranscriptMomentErrorReason::BatchContentKeyMismatch,
        ),
        MomentBatchError::AnalyzerIdentityMismatch { record_index } => (
            record_index,
            TranscriptMomentErrorReason::BatchAnalyzerIdentityMismatch,
        ),
        MomentBatchError::AnalyzerVersionMismatch { record_index } => (
            record_index,
            TranscriptMomentErrorReason::BatchAnalyzerVersionMismatch,
        ),
        MomentBatchError::DuplicateRecord {
            duplicate_index, ..
        } => (
            duplicate_index,
            TranscriptMomentErrorReason::BatchDuplicateRecord,
        ),
    };

    TranscriptMomentConversionError::batch(record_indices.get(record_position).copied(), reason)
}

#[cfg(test)]
mod tests {
    use cutlass_analysis::moments::{
        AnalyzerIdentity, AnalyzerVersion, MAX_MOMENT_PAYLOAD_BYTES, MediaContentKey,
        MomentConfidence, MomentKind,
    };
    use serde_json::Value;

    use super::*;
    use crate::{TranscriptSegment, TranscriptWord};

    fn content_key() -> MediaContentKey {
        MediaContentKey::from_bytes([0x5a; 32])
    }

    fn analyzer_identity() -> AnalyzerIdentity {
        AnalyzerIdentity::new("whisper/base.en").expect("fixture identity is valid")
    }

    fn analyzer_version() -> AnalyzerVersion {
        AnalyzerVersion::new(9).expect("fixture version is valid")
    }

    fn convert(
        transcript: &Transcript,
    ) -> Result<MomentReplacementBatch, TranscriptMomentConversionError> {
        transcript_to_moment_batch(
            content_key(),
            analyzer_identity(),
            analyzer_version(),
            transcript,
        )
    }

    #[test]
    fn mixed_transcript_emits_sorted_records_with_caller_provenance() {
        let transcript = Transcript::test_fixture(vec![
            TranscriptSegment::test_fixture(
                100,
                200,
                "Alpha beta",
                vec![
                    TranscriptWord::test_fixture(100, 150, "Alpha", 0.75),
                    TranscriptWord::test_fixture(150, 200, "beta", 0.25),
                ],
            ),
            TranscriptSegment::test_fixture(200, 250, "", Vec::new()),
            TranscriptSegment::test_fixture(
                300,
                350,
                "Gamma",
                vec![TranscriptWord::test_fixture(300, 350, "Gamma", 0.875)],
            ),
        ]);

        let batch = convert(&transcript).expect("fixture transcript converts");
        let expected = [
            (
                "transcript_word",
                1.0,
                1.5,
                0.75,
                r#"{"v":1,"segment_index":0,"word_index":0,"text":"Alpha"}"#,
            ),
            (
                "transcript_segment",
                1.0,
                2.0,
                0.5,
                r#"{"v":1,"segment_index":0,"text":"Alpha beta"}"#,
            ),
            (
                "transcript_word",
                1.5,
                2.0,
                0.25,
                r#"{"v":1,"segment_index":0,"word_index":1,"text":"beta"}"#,
            ),
            (
                "transcript_segment",
                3.0,
                3.5,
                0.875,
                r#"{"v":1,"segment_index":2,"text":"Gamma"}"#,
            ),
            (
                "transcript_word",
                3.0,
                3.5,
                0.875,
                r#"{"v":1,"segment_index":2,"word_index":0,"text":"Gamma"}"#,
            ),
        ];

        assert_eq!(batch.key().content_key(), content_key());
        assert_eq!(batch.key().analyzer_identity(), &analyzer_identity());
        assert_eq!(batch.key().analyzer_version(), analyzer_version());
        assert_eq!(batch.records().len(), expected.len());

        for (record, (kind, start, end, confidence, payload)) in
            batch.records().iter().zip(expected)
        {
            assert_eq!(record.kind().as_str(), kind);
            assert_eq!(record.span().start_seconds(), start);
            assert_eq!(record.span().end_seconds(), end);
            assert_eq!(record.confidence().get(), confidence);
            assert_eq!(record.content_key(), content_key());
            assert_eq!(record.analyzer_identity(), &analyzer_identity());
            assert_eq!(record.analyzer_version(), analyzer_version());
            assert_eq!(
                record
                    .payload()
                    .expect("transcript records have payloads")
                    .as_str(),
                payload
            );
        }
    }

    #[test]
    fn repeated_identical_words_remain_distinct_by_ordinal() {
        let transcript = Transcript::test_fixture(vec![TranscriptSegment::test_fixture(
            0,
            100,
            "echo echo",
            vec![
                TranscriptWord::test_fixture(10, 20, "echo", 0.5),
                TranscriptWord::test_fixture(10, 20, "echo", 0.5),
            ],
        )]);

        let batch = convert(&transcript).expect("ordinals prevent duplicate records");
        let word_payloads: Vec<_> = batch
            .records()
            .iter()
            .filter(|record| record.kind() == &MomentKind::TRANSCRIPT_WORD)
            .map(|record| {
                record
                    .payload()
                    .expect("word has payload")
                    .as_str()
                    .to_owned()
            })
            .collect();

        assert_eq!(word_payloads.len(), 2);
        assert_ne!(word_payloads[0], word_payloads[1]);
        assert!(word_payloads[0].contains(r#""word_index":0"#));
        assert!(word_payloads[1].contains(r#""word_index":1"#));
    }

    #[test]
    fn arbitrary_text_round_trips_through_canonical_json_payloads() {
        let text = "quote \" backslash \\ tab\tcr\rlf\nUnicode: 雪 🦀";
        let transcript = Transcript::test_fixture(vec![TranscriptSegment::test_fixture(
            10,
            20,
            text,
            vec![TranscriptWord::test_fixture(10, 20, text, 0.5)],
        )]);

        let batch = convert(&transcript).expect("escaped text converts");
        assert_eq!(batch.records().len(), 2);
        for record in batch.records() {
            let encoded = record.payload().expect("record has payload").as_str();
            let decoded: Value = serde_json::from_str(encoded).expect("payload is valid JSON");
            assert_eq!(decoded.get("v").and_then(Value::as_u64), Some(1));
            assert_eq!(decoded.get("text").and_then(Value::as_str), Some(text));
            assert!(!encoded.contains('\t'));
            assert!(!encoded.contains('\r'));
            assert!(!encoded.contains('\n'));
        }
    }

    #[test]
    fn empty_transcript_yields_valid_empty_replacement_marker() {
        let transcript = Transcript::test_fixture(Vec::new());
        let batch = convert(&transcript).expect("empty transcript is valid");

        assert!(batch.is_empty());
        assert_eq!(batch.key().content_key(), content_key());
        assert_eq!(batch.key().analyzer_identity(), &analyzer_identity());
        assert_eq!(batch.key().analyzer_version(), analyzer_version());
    }

    #[test]
    fn segment_without_words_uses_conservative_fallback_confidence() {
        let transcript = Transcript::test_fixture(vec![TranscriptSegment::test_fixture(
            25,
            75,
            "No token timestamps",
            Vec::new(),
        )]);

        let batch = convert(&transcript).expect("no-word segment converts");
        assert_eq!(batch.records().len(), 1);
        assert_eq!(
            batch.records()[0].confidence(),
            TRANSCRIPT_SEGMENT_FALLBACK_CONFIDENCE
        );
        assert_eq!(batch.records()[0].confidence(), MomentConfidence::ZERO);
    }

    #[test]
    fn oversized_payload_fails_with_bounded_non_leaking_error() {
        let secret = "private transcript marker";
        let text = format!("{secret}{}", "x".repeat(MAX_MOMENT_PAYLOAD_BYTES));
        let transcript = Transcript::test_fixture(vec![TranscriptSegment::test_fixture(
            0,
            1,
            text,
            Vec::new(),
        )]);

        let error = convert(&transcript).expect_err("oversized payload must fail closed");
        assert_eq!(
            error.record_index(),
            Some(TranscriptMomentRecordIndex::Segment { segment_index: 0 })
        );
        assert_eq!(error.field(), TranscriptMomentErrorField::Payload);
        assert_eq!(
            error.reason(),
            TranscriptMomentErrorReason::PayloadExceedsLimit
        );
        let rendered = error.to_string();
        assert!(!rendered.contains(secret));
        assert!(rendered.len() < 200);
    }

    #[test]
    fn output_is_repeatable_and_independent_of_string_capacity() {
        let compact_text = String::from("same allocation-independent text");
        let mut spare_capacity_text = String::with_capacity(4_096);
        spare_capacity_text.push_str(&compact_text);

        let compact = Transcript::test_fixture(vec![TranscriptSegment::test_fixture(
            40,
            80,
            compact_text,
            vec![TranscriptWord::test_fixture(40, 80, "same", 0.625)],
        )]);
        let spare_capacity = Transcript::test_fixture(vec![TranscriptSegment::test_fixture(
            40,
            80,
            spare_capacity_text,
            vec![TranscriptWord::test_fixture(40, 80, "same", 0.625)],
        )]);

        let first = convert(&compact).expect("first conversion succeeds");
        let repeated = convert(&compact).expect("repeated conversion succeeds");
        let reallocated = convert(&spare_capacity).expect("equivalent allocation converts");
        assert_eq!(first, repeated);
        assert_eq!(first, reallocated);
    }

    #[test]
    fn unrepresentable_centiseconds_fail_closed() {
        let transcript = Transcript::test_fixture(vec![TranscriptSegment::test_fixture(
            MAX_EXACT_F64_CENTISECONDS + 1,
            MAX_EXACT_F64_CENTISECONDS + 1,
            "far future",
            Vec::new(),
        )]);

        let error = convert(&transcript).expect_err("precision-losing boundary must fail");
        assert_eq!(
            error.record_index(),
            Some(TranscriptMomentRecordIndex::Segment { segment_index: 0 })
        );
        assert_eq!(error.field(), TranscriptMomentErrorField::StartCentiseconds);
        assert_eq!(
            error.reason(),
            TranscriptMomentErrorReason::CentisecondsNotRepresentable
        );
    }
}
