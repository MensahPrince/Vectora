//! Adapters from deterministic analysis events to atomic moment replacements.
//!
//! These adapters deliberately accept caller-selected content, analyzer, and
//! version identifiers. Detector configuration and wider provenance remain an
//! application concern rather than being inferred or serialized here.
//!
//! Beat and shot-boundary records are point events: their [`TimeSpan`] start
//! and end both equal the event timestamp. Silence records retain the detector's
//! exact half-open span. Beat and silence confidence is
//! [`MomentConfidence::CERTAIN`], meaning only that the deterministic detector
//! emitted the record; it is not a claim of semantic certainty. Shot confidence
//! is the detector's normalized candidate score, which likewise is not a
//! calibrated probability.
//!
//! # Payload schema
//!
//! Payloads are canonical compact JSON text with keys in the documented order:
//!
//! - beat: `{"v":1,"index":N,"strength_db":X}`
//! - silence: `{"v":1,"index":N}`
//! - shot boundary:
//!   `{"v":1,"index":N,"kind":"visual_change|dimension_change",` followed by
//!   `"previous_seconds":X,"current_seconds":Y}`
//!
//! `index` is the zero-based source ordinal. It preserves otherwise identical
//! source records as distinct moments and gives their payloads deterministic
//! identities. Finite floating-point values use Rust's shortest round-trippable
//! number formatting. Changing these fields, their order, or their semantics
//! requires incrementing [`MOMENT_ADAPTER_PAYLOAD_VERSION`].
//!
//! # Complexity and allocations
//!
//! Record validation and payload construction are one `O(n)` pass. The record
//! vector is preallocated for exactly `n` entries, and each input produces one
//! bounded payload and one owned record. Final construction follows
//! [`MomentReplacementBatch`]'s canonical-order contract, adding its documented
//! `O(n log n)` sort and `O(n)` scratch storage. No PCM or frame data is copied.
//! Records are supplied to the batch constructor in input order; returned
//! records follow the batch's canonical order.

use std::error::Error;
use std::fmt;

use crate::{BeatEvent, ShotBoundary, ShotBoundaryKind, SilenceSpan, TimeSpan, TimeSpanError};

use super::{
    AnalyzerIdentity, AnalyzerVersion, MediaContentKey, MomentBatchError, MomentBatchKey,
    MomentConfidence, MomentConfidenceError, MomentKind, MomentPayload, MomentPayloadError,
    MomentRecord, MomentReplacementBatch,
};

/// Current schema version embedded in every deterministic-adapter payload.
pub const MOMENT_ADAPTER_PAYLOAD_VERSION: u32 = 1;

const FIELD_BATCH: &str = "batch";
const FIELD_PAYLOAD: &str = "payload";
const REASON_FINITE: &str = "must be finite";
const REASON_NON_NEGATIVE: &str = "must be non-negative";
const REASON_POSITIVE: &str = "must be positive";

/// The deterministic source record being converted when an adapter fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterRecordKind {
    /// A [`BeatEvent`].
    Beat,
    /// A [`SilenceSpan`].
    Silence,
    /// A [`ShotBoundary`].
    ShotBoundary,
}

impl AdapterRecordKind {
    /// Returns the stable built-in moment-kind name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Beat => "beat",
            Self::Silence => "silence",
            Self::ShotBoundary => "shot_boundary",
        }
    }
}

impl fmt::Display for AdapterRecordKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A bounded failure while converting one deterministic analysis record.
///
/// The error retains only a typed record kind, source ordinal, and static
/// field/reason strings. It never captures an input value or generated payload,
/// keeping diagnostics bounded even for untrusted externally decoded data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MomentAdapterError {
    record_kind: AdapterRecordKind,
    record_index: usize,
    field: &'static str,
    reason: &'static str,
}

impl MomentAdapterError {
    const fn new(
        record_kind: AdapterRecordKind,
        record_index: usize,
        field: &'static str,
        reason: &'static str,
    ) -> Self {
        Self {
            record_kind,
            record_index,
            field,
            reason,
        }
    }

    /// Returns the deterministic source record kind.
    #[must_use]
    pub const fn record_kind(&self) -> AdapterRecordKind {
        self.record_kind
    }

    /// Returns the zero-based ordinal in the borrowed source slice.
    #[must_use]
    pub const fn record_index(&self) -> usize {
        self.record_index
    }

    /// Returns the stable field name associated with the failure.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    /// Returns the stable reason associated with the failure.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for MomentAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot adapt {} record at index {}: `{}` {}",
            self.record_kind, self.record_index, self.field, self.reason
        )
    }
}

impl Error for MomentAdapterError {}

/// Converts borrowed beat events into one complete atomic replacement batch.
///
/// Each beat becomes a point event of kind [`MomentKind::BEAT`] with
/// [`MomentConfidence::CERTAIN`]. The payload retains the positive finite
/// `strength_db` without mapping decibels to a fictional probability.
/// Caller-supplied analyzer identity/version define the replacement scope and
/// should represent the detector configuration and provenance policy chosen by
/// the application.
///
/// Empty input returns a valid empty batch, allowing storage to remove old rows
/// in this exact replacement scope.
///
/// # Errors
///
/// Returns [`MomentAdapterError`] for a non-finite or negative event timestamp,
/// a non-finite or non-positive strength, or any downstream span, payload, or
/// batch validation failure.
pub fn beat_events_to_moment_batch(
    events: &[BeatEvent],
    content_key: MediaContentKey,
    analyzer_identity: AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
) -> Result<MomentReplacementBatch, MomentAdapterError> {
    let record_kind = AdapterRecordKind::Beat;
    let mut records = Vec::with_capacity(events.len());

    for (index, event) in events.iter().copied().enumerate() {
        let timestamp =
            validate_non_negative_seconds(record_kind, index, "time_seconds", event.time_seconds)?;
        validate_positive_f32(record_kind, index, "strength_db", event.strength_db)?;
        let span = point_span(record_kind, index, "time_seconds", timestamp)?;
        let payload = payload(
            record_kind,
            index,
            format!(
                r#"{{"v":{MOMENT_ADAPTER_PAYLOAD_VERSION},"index":{index},"strength_db":{}}}"#,
                event.strength_db
            ),
        )?;

        records.push(MomentRecord::new(
            content_key,
            span,
            MomentKind::BEAT,
            MomentConfidence::CERTAIN,
            analyzer_identity.clone(),
            analyzer_version,
            Some(payload),
        ));
    }

    finish_batch(
        record_kind,
        content_key,
        analyzer_identity,
        analyzer_version,
        records,
    )
}

/// Converts borrowed silence spans into one complete atomic replacement batch.
///
/// Each record keeps the exact detected half-open span, uses kind
/// [`MomentKind::SILENCE`], and has [`MomentConfidence::CERTAIN`] to state that
/// the deterministic detector emitted it. The source ordinal payload keeps
/// duplicate spans distinct.
///
/// Empty input returns a valid empty batch, allowing storage to remove old rows
/// in this exact caller-selected replacement scope.
///
/// # Errors
///
/// Returns [`MomentAdapterError`] for any span, payload, or batch validation
/// failure.
pub fn silence_spans_to_moment_batch(
    spans: &[SilenceSpan],
    content_key: MediaContentKey,
    analyzer_identity: AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
) -> Result<MomentReplacementBatch, MomentAdapterError> {
    let record_kind = AdapterRecordKind::Silence;
    let mut records = Vec::with_capacity(spans.len());

    for (index, silence) in spans.iter().copied().enumerate() {
        let span = validate_span(record_kind, index, silence.span)?;
        let payload = payload(
            record_kind,
            index,
            format!(r#"{{"v":{MOMENT_ADAPTER_PAYLOAD_VERSION},"index":{index}}}"#),
        )?;

        records.push(MomentRecord::new(
            content_key,
            span,
            MomentKind::SILENCE,
            MomentConfidence::CERTAIN,
            analyzer_identity.clone(),
            analyzer_version,
            Some(payload),
        ));
    }

    finish_batch(
        record_kind,
        content_key,
        analyzer_identity,
        analyzer_version,
        records,
    )
}

/// Converts borrowed shot boundaries into one complete atomic replacement.
///
/// Each boundary becomes a point event of kind
/// [`MomentKind::SHOT_BOUNDARY`]. Its normalized detector candidate score is
/// retained directly as confidence; no semantic-probability interpretation is
/// added. The payload retains boundary kind and both sampled timestamps.
///
/// Timestamp finiteness, non-negativity, ordering, and equality between the
/// boundary timestamp and current sampled timestamp are checked defensively,
/// even though current detector constructors already enforce those invariants.
/// Empty input returns a valid empty replacement batch.
///
/// # Errors
///
/// Returns [`MomentAdapterError`] for inconsistent timestamps, an invalid
/// confidence, or any downstream span, payload, or batch validation failure.
pub fn shot_boundaries_to_moment_batch(
    boundaries: &[ShotBoundary],
    content_key: MediaContentKey,
    analyzer_identity: AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
) -> Result<MomentReplacementBatch, MomentAdapterError> {
    let record_kind = AdapterRecordKind::ShotBoundary;
    let mut records = Vec::with_capacity(boundaries.len());

    for (index, boundary) in boundaries.iter().copied().enumerate() {
        let timestamp = validate_non_negative_seconds(
            record_kind,
            index,
            "timestamp_seconds",
            boundary.timestamp_seconds(),
        )?;
        let previous_seconds = validate_non_negative_seconds(
            record_kind,
            index,
            "previous_frame_timestamp_seconds",
            boundary.previous_frame_timestamp_seconds(),
        )?;
        let current_seconds = validate_non_negative_seconds(
            record_kind,
            index,
            "current_frame_timestamp_seconds",
            boundary.current_frame_timestamp_seconds(),
        )?;
        if previous_seconds > current_seconds {
            return Err(MomentAdapterError::new(
                record_kind,
                index,
                "current_frame_timestamp_seconds",
                "must not precede previous sampled timestamp",
            ));
        }
        if timestamp != current_seconds {
            return Err(MomentAdapterError::new(
                record_kind,
                index,
                "timestamp_seconds",
                "must equal current sampled timestamp",
            ));
        }

        let confidence = MomentConfidence::new(boundary.score())
            .map_err(|error| confidence_error(record_kind, index, "score", error))?;
        let span = point_span(record_kind, index, "timestamp_seconds", timestamp)?;
        let boundary_kind = shot_boundary_kind_name(boundary.kind());
        let payload = payload(
            record_kind,
            index,
            format!(
                r#"{{"v":{MOMENT_ADAPTER_PAYLOAD_VERSION},"index":{index},"kind":"{boundary_kind}","previous_seconds":{previous_seconds},"current_seconds":{current_seconds}}}"#
            ),
        )?;

        records.push(MomentRecord::new(
            content_key,
            span,
            MomentKind::SHOT_BOUNDARY,
            confidence,
            analyzer_identity.clone(),
            analyzer_version,
            Some(payload),
        ));
    }

    finish_batch(
        record_kind,
        content_key,
        analyzer_identity,
        analyzer_version,
        records,
    )
}

fn validate_non_negative_seconds(
    record_kind: AdapterRecordKind,
    record_index: usize,
    field: &'static str,
    value: f64,
) -> Result<f64, MomentAdapterError> {
    if !value.is_finite() {
        return Err(MomentAdapterError::new(
            record_kind,
            record_index,
            field,
            REASON_FINITE,
        ));
    }
    if value < 0.0 {
        return Err(MomentAdapterError::new(
            record_kind,
            record_index,
            field,
            REASON_NON_NEGATIVE,
        ));
    }

    Ok(if value == 0.0 { 0.0 } else { value })
}

fn validate_positive_f32(
    record_kind: AdapterRecordKind,
    record_index: usize,
    field: &'static str,
    value: f32,
) -> Result<(), MomentAdapterError> {
    if !value.is_finite() {
        return Err(MomentAdapterError::new(
            record_kind,
            record_index,
            field,
            REASON_FINITE,
        ));
    }
    if value <= 0.0 {
        return Err(MomentAdapterError::new(
            record_kind,
            record_index,
            field,
            REASON_POSITIVE,
        ));
    }
    Ok(())
}

fn point_span(
    record_kind: AdapterRecordKind,
    record_index: usize,
    field: &'static str,
    timestamp: f64,
) -> Result<TimeSpan, MomentAdapterError> {
    TimeSpan::new(timestamp, timestamp)
        .map_err(|error| span_error(record_kind, record_index, field, error))
}

fn validate_span(
    record_kind: AdapterRecordKind,
    record_index: usize,
    span: TimeSpan,
) -> Result<TimeSpan, MomentAdapterError> {
    TimeSpan::new(span.start_seconds(), span.end_seconds())
        .map_err(|error| span_error(record_kind, record_index, "span", error))
}

fn span_error(
    record_kind: AdapterRecordKind,
    record_index: usize,
    field: &'static str,
    error: TimeSpanError,
) -> MomentAdapterError {
    MomentAdapterError::new(record_kind, record_index, field, error.reason())
}

fn confidence_error(
    record_kind: AdapterRecordKind,
    record_index: usize,
    field: &'static str,
    error: MomentConfidenceError,
) -> MomentAdapterError {
    let reason = match error {
        MomentConfidenceError::NonFinite => REASON_FINITE,
        MomentConfidenceError::OutOfRange => "must be in the inclusive range [0, 1]",
    };
    MomentAdapterError::new(record_kind, record_index, field, reason)
}

fn payload(
    record_kind: AdapterRecordKind,
    record_index: usize,
    text: String,
) -> Result<MomentPayload, MomentAdapterError> {
    MomentPayload::new(&text).map_err(|error| payload_error(record_kind, record_index, error))
}

fn payload_error(
    record_kind: AdapterRecordKind,
    record_index: usize,
    error: MomentPayloadError,
) -> MomentAdapterError {
    let reason = match error {
        MomentPayloadError::Empty => "must not be empty",
        MomentPayloadError::TooLong { .. } => "exceeds the maximum payload byte length",
        MomentPayloadError::DisallowedControl { .. } => "contains a disallowed control character",
    };
    MomentAdapterError::new(record_kind, record_index, FIELD_PAYLOAD, reason)
}

fn shot_boundary_kind_name(kind: ShotBoundaryKind) -> &'static str {
    match kind {
        ShotBoundaryKind::VisualChange => "visual_change",
        ShotBoundaryKind::DimensionChange => "dimension_change",
    }
}

fn finish_batch(
    record_kind: AdapterRecordKind,
    content_key: MediaContentKey,
    analyzer_identity: AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
    records: Vec<MomentRecord>,
) -> Result<MomentReplacementBatch, MomentAdapterError> {
    let key = MomentBatchKey::new(content_key, analyzer_identity, analyzer_version);
    MomentReplacementBatch::new(key, records).map_err(|error| batch_error(record_kind, error))
}

fn batch_error(record_kind: AdapterRecordKind, error: MomentBatchError) -> MomentAdapterError {
    let (record_index, reason) = match error {
        MomentBatchError::ContentKeyMismatch { record_index } => {
            (record_index, "record content key differs from batch scope")
        }
        MomentBatchError::AnalyzerIdentityMismatch { record_index } => (
            record_index,
            "record analyzer identity differs from batch scope",
        ),
        MomentBatchError::AnalyzerVersionMismatch { record_index } => (
            record_index,
            "record analyzer version differs from batch scope",
        ),
        MomentBatchError::DuplicateRecord {
            duplicate_index, ..
        } => (duplicate_index, "duplicates an earlier batch record"),
    };
    MomentAdapterError::new(record_kind, record_index, FIELD_BATCH, reason)
}
