//! Validated domain and query types for persistent media moments.
//!
//! The domain values and in-memory matching behavior remain dependency-free.
//! Enabling the `sqlite` feature also exposes the optional persistent
//! [`MomentsIndex`] adapter.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use crate::TimeSpan;

pub mod adapters;

pub use adapters::{
    AdapterRecordKind, MOMENT_ADAPTER_PAYLOAD_VERSION, MomentAdapterError,
    beat_events_to_moment_batch, shot_boundaries_to_moment_batch, silence_spans_to_moment_batch,
};

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "sqlite")]
pub use sqlite::{
    DEFAULT_MOMENTS_BUSY_TIMEOUT, DEFAULT_MOMENTS_QUERY_LIMIT, MAX_MOMENT_TIME_SECONDS,
    MAX_MOMENTS_BUSY_TIMEOUT, MAX_MOMENTS_QUERY_LIMIT, MOMENTS_INDEX_DATABASE_FILENAME,
    MOMENTS_INDEX_SCHEMA_VERSION, MomentsIndex, MomentsIndexConfig, MomentsIndexError,
    MomentsQueryResult,
};

/// Number of bytes in a [`MediaContentKey`].
pub const MEDIA_CONTENT_KEY_BYTES: usize = 32;

/// Number of ASCII bytes in the hexadecimal form of a [`MediaContentKey`].
pub const MEDIA_CONTENT_KEY_HEX_BYTES: usize = MEDIA_CONTENT_KEY_BYTES * 2;

/// Maximum UTF-8 byte length of a [`MomentKind`].
pub const MAX_MOMENT_KIND_BYTES: usize = 64;

/// Maximum UTF-8 byte length of an [`AnalyzerIdentity`].
pub const MAX_ANALYZER_IDENTITY_BYTES: usize = 128;

/// Maximum UTF-8 byte length of a [`MomentPayload`].
///
/// The payload is intentionally small enough for bounded tool and database
/// results. Larger analyzer artifacts should live outside the moments index.
pub const MAX_MOMENT_PAYLOAD_BYTES: usize = 16 * 1024;

const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

/// A caller-supplied, fixed-size media content digest.
///
/// The key stores exactly 32 bytes inline and performs no allocation. This
/// crate does not choose a digest algorithm or hash media; callers must supply
/// the digest and use one algorithm consistently within an index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaContentKey([u8; MEDIA_CONTENT_KEY_BYTES]);

impl MediaContentKey {
    /// Constructs a key from an exactly sized byte array.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; MEDIA_CONTENT_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// Copies a key from a byte slice of exactly 32 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MediaContentKeyError::InvalidByteLength`] when `bytes` does
    /// not contain exactly [`MEDIA_CONTENT_KEY_BYTES`] bytes.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, MediaContentKeyError> {
        if bytes.len() != MEDIA_CONTENT_KEY_BYTES {
            return Err(MediaContentKeyError::InvalidByteLength {
                actual: bytes.len(),
            });
        }

        let mut key = [0_u8; MEDIA_CONTENT_KEY_BYTES];
        key.copy_from_slice(bytes);
        Ok(Self(key))
    }

    /// Returns the exact digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; MEDIA_CONTENT_KEY_BYTES] {
        &self.0
    }

    /// Consumes the key and returns the exact digest bytes.
    #[must_use]
    pub const fn into_bytes(self) -> [u8; MEDIA_CONTENT_KEY_BYTES] {
        self.0
    }

    /// Encodes the key as 64 lowercase hexadecimal ASCII bytes.
    ///
    /// The returned array is stack/inline data and requires no heap
    /// allocation. [`fmt::Display`] emits the same canonical representation.
    #[must_use]
    pub fn encode_hex(self) -> [u8; MEDIA_CONTENT_KEY_HEX_BYTES] {
        let mut encoded = [0_u8; MEDIA_CONTENT_KEY_HEX_BYTES];
        for (index, byte) in self.0.iter().copied().enumerate() {
            encoded[index * 2] = LOWER_HEX[usize::from(byte >> 4)];
            encoded[index * 2 + 1] = LOWER_HEX[usize::from(byte & 0x0f)];
        }
        encoded
    }
}

impl From<[u8; MEDIA_CONTENT_KEY_BYTES]> for MediaContentKey {
    fn from(bytes: [u8; MEDIA_CONTENT_KEY_BYTES]) -> Self {
        Self::from_bytes(bytes)
    }
}

impl TryFrom<&[u8]> for MediaContentKey {
    type Error = MediaContentKeyError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::from_slice(bytes)
    }
}

impl fmt::Display for MediaContentKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let encoded = self.encode_hex();
        let text = std::str::from_utf8(&encoded).map_err(|_| fmt::Error)?;
        formatter.write_str(text)
    }
}

impl FromStr for MediaContentKey {
    type Err = MediaContentKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let encoded = value.as_bytes();
        if encoded.len() != MEDIA_CONTENT_KEY_HEX_BYTES {
            return Err(MediaContentKeyError::InvalidHexLength {
                actual: encoded.len(),
            });
        }

        let mut bytes = [0_u8; MEDIA_CONTENT_KEY_BYTES];
        for (index, pair) in encoded.chunks_exact(2).enumerate() {
            let high = decode_hex_digit(pair[0], index * 2)?;
            let low = decode_hex_digit(pair[1], index * 2 + 1)?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

/// Errors produced while constructing or parsing a [`MediaContentKey`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaContentKeyError {
    /// A binary digest did not contain exactly 32 bytes.
    InvalidByteLength {
        /// Actual number of supplied bytes.
        actual: usize,
    },
    /// A hexadecimal digest did not contain exactly 64 ASCII bytes.
    InvalidHexLength {
        /// Actual UTF-8 byte length of the supplied string.
        actual: usize,
    },
    /// A hexadecimal digest contained a byte outside `0-9`, `a-f`, and `A-F`.
    InvalidHexCharacter {
        /// Zero-based byte position in the supplied string.
        index: usize,
        /// Invalid byte found at `index`.
        byte: u8,
    },
}

impl fmt::Display for MediaContentKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidByteLength { actual } => write!(
                formatter,
                "media content key has {actual} bytes; expected {MEDIA_CONTENT_KEY_BYTES}"
            ),
            Self::InvalidHexLength { actual } => write!(
                formatter,
                "hex media content key has {actual} bytes; expected \
                 {MEDIA_CONTENT_KEY_HEX_BYTES}"
            ),
            Self::InvalidHexCharacter { index, byte } => write!(
                formatter,
                "invalid hexadecimal byte 0x{byte:02x} at byte index {index}"
            ),
        }
    }
}

impl Error for MediaContentKeyError {}

fn decode_hex_digit(byte: u8, index: usize) -> Result<u8, MediaContentKeyError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(MediaContentKeyError::InvalidHexCharacter { index, byte }),
    }
}

/// A validated, storage-stable moment kind.
///
/// Names use lowercase ASCII identifiers: the first byte is `a-z`, and later
/// bytes may additionally be digits, `.`, `_`, or `-`. Dots allow custom
/// analyzers to namespace kinds without expanding a closed enum. Built-in
/// values are stored as borrowed static strings; custom values are owned.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MomentKind(Cow<'static, str>);

impl MomentKind {
    /// Stable storage name for a detected shot boundary.
    pub const SHOT_BOUNDARY: Self = Self(Cow::Borrowed("shot_boundary"));

    /// Stable storage name for a silent span.
    pub const SILENCE: Self = Self(Cow::Borrowed("silence"));

    /// Stable storage name for a beat or onset point.
    pub const BEAT: Self = Self(Cow::Borrowed("beat"));

    /// Stable storage name for a transcript segment.
    pub const TRANSCRIPT_SEGMENT: Self = Self(Cow::Borrowed("transcript_segment"));

    /// Stable storage name for a transcript word.
    pub const TRANSCRIPT_WORD: Self = Self(Cow::Borrowed("transcript_word"));

    /// Stable storage name for a semantically detected visual moment.
    pub const VISION_MOMENT: Self = Self(Cow::Borrowed("vision_moment"));

    /// Validates a built-in or custom storage name.
    ///
    /// Known built-in names reuse static storage. Other valid names are copied
    /// into an owned string.
    ///
    /// # Errors
    ///
    /// Returns [`MomentKindError`] when `name` is empty, too long, or outside
    /// the documented lowercase ASCII grammar.
    pub fn new(name: &str) -> Result<Self, MomentKindError> {
        validate_moment_kind(name)?;
        Ok(match name {
            "shot_boundary" => Self::SHOT_BOUNDARY,
            "silence" => Self::SILENCE,
            "beat" => Self::BEAT,
            "transcript_segment" => Self::TRANSCRIPT_SEGMENT,
            "transcript_word" => Self::TRANSCRIPT_WORD,
            "vision_moment" => Self::VISION_MOMENT,
            _ => Self(Cow::Owned(name.to_owned())),
        })
    }

    /// Returns the stable name used by storage and query adapters.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    /// Returns whether this value is one of the built-in kinds.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        matches!(
            self.as_str(),
            "shot_boundary"
                | "silence"
                | "beat"
                | "transcript_segment"
                | "transcript_word"
                | "vision_moment"
        )
    }
}

impl FromStr for MomentKind {
    type Err = MomentKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for MomentKind {
    type Error = MomentKindError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl AsRef<str> for MomentKind {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for MomentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Validation errors for a [`MomentKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MomentKindError {
    /// The name was empty.
    Empty,
    /// The UTF-8 representation exceeded [`MAX_MOMENT_KIND_BYTES`].
    TooLong {
        /// Actual UTF-8 byte length.
        actual: usize,
    },
    /// The first byte was not a lowercase ASCII letter.
    InvalidStart {
        /// Invalid first byte.
        byte: u8,
    },
    /// A later byte was outside the accepted identifier grammar.
    InvalidCharacter {
        /// Zero-based byte position in the name.
        index: usize,
        /// Invalid byte found at `index`.
        byte: u8,
    },
}

impl fmt::Display for MomentKindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("moment kind must not be empty"),
            Self::TooLong { actual } => write!(
                formatter,
                "moment kind has {actual} bytes; maximum is {MAX_MOMENT_KIND_BYTES}"
            ),
            Self::InvalidStart { byte } => write!(
                formatter,
                "moment kind must start with a lowercase ASCII letter, found 0x{byte:02x}"
            ),
            Self::InvalidCharacter { index, byte } => write!(
                formatter,
                "moment kind contains invalid byte 0x{byte:02x} at byte index {index}"
            ),
        }
    }
}

impl Error for MomentKindError {}

fn validate_moment_kind(name: &str) -> Result<(), MomentKindError> {
    let bytes = name.as_bytes();
    let Some((&first, remaining)) = bytes.split_first() else {
        return Err(MomentKindError::Empty);
    };
    if bytes.len() > MAX_MOMENT_KIND_BYTES {
        return Err(MomentKindError::TooLong {
            actual: bytes.len(),
        });
    }
    if !first.is_ascii_lowercase() {
        return Err(MomentKindError::InvalidStart { byte: first });
    }

    for (offset, byte) in remaining.iter().copied().enumerate() {
        if !(byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(MomentKindError::InvalidCharacter {
                index: offset + 1,
                byte,
            });
        }
    }
    Ok(())
}

/// A validated, stable analyzer or source identity.
///
/// Identities use lowercase ASCII. The first byte is a letter or digit; later
/// bytes may additionally be `.`, `_`, `-`, or `/`. This permits namespaced
/// custom analyzers while keeping database and tool inputs bounded and
/// unambiguous.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnalyzerIdentity(String);

impl AnalyzerIdentity {
    /// Validates and copies an analyzer identity.
    ///
    /// # Errors
    ///
    /// Returns [`AnalyzerIdentityError`] when `identity` is empty, too long,
    /// or outside the documented lowercase ASCII grammar.
    pub fn new(identity: &str) -> Result<Self, AnalyzerIdentityError> {
        validate_analyzer_identity(identity)?;
        Ok(Self(identity.to_owned()))
    }

    /// Returns the stable storage identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for AnalyzerIdentity {
    type Err = AnalyzerIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for AnalyzerIdentity {
    type Error = AnalyzerIdentityError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl AsRef<str> for AnalyzerIdentity {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for AnalyzerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Validation errors for an [`AnalyzerIdentity`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnalyzerIdentityError {
    /// The identity was empty.
    Empty,
    /// The UTF-8 representation exceeded [`MAX_ANALYZER_IDENTITY_BYTES`].
    TooLong {
        /// Actual UTF-8 byte length.
        actual: usize,
    },
    /// The first byte was not a lowercase ASCII letter or digit.
    InvalidStart {
        /// Invalid first byte.
        byte: u8,
    },
    /// A later byte was outside the accepted identifier grammar.
    InvalidCharacter {
        /// Zero-based byte position in the identity.
        index: usize,
        /// Invalid byte found at `index`.
        byte: u8,
    },
}

impl fmt::Display for AnalyzerIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("analyzer identity must not be empty"),
            Self::TooLong { actual } => write!(
                formatter,
                "analyzer identity has {actual} bytes; maximum is \
                 {MAX_ANALYZER_IDENTITY_BYTES}"
            ),
            Self::InvalidStart { byte } => write!(
                formatter,
                "analyzer identity must start with a lowercase ASCII letter or digit, found \
                 0x{byte:02x}"
            ),
            Self::InvalidCharacter { index, byte } => write!(
                formatter,
                "analyzer identity contains invalid byte 0x{byte:02x} at byte index {index}"
            ),
        }
    }
}

impl Error for AnalyzerIdentityError {}

fn validate_analyzer_identity(identity: &str) -> Result<(), AnalyzerIdentityError> {
    let bytes = identity.as_bytes();
    let Some((&first, remaining)) = bytes.split_first() else {
        return Err(AnalyzerIdentityError::Empty);
    };
    if bytes.len() > MAX_ANALYZER_IDENTITY_BYTES {
        return Err(AnalyzerIdentityError::TooLong {
            actual: bytes.len(),
        });
    }
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(AnalyzerIdentityError::InvalidStart { byte: first });
    }

    for (offset, byte) in remaining.iter().copied().enumerate() {
        if !(byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'_' | b'-' | b'/'))
        {
            return Err(AnalyzerIdentityError::InvalidCharacter {
                index: offset + 1,
                byte,
            });
        }
    }
    Ok(())
}

/// A non-zero analyzer result schema/version.
///
/// Adapters can store this value as a positive 32-bit integer. An analyzer
/// must change the version whenever persisted payload or result semantics
/// change incompatibly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnalyzerVersion(NonZeroU32);

impl AnalyzerVersion {
    /// Constructs a positive analyzer version.
    ///
    /// # Errors
    ///
    /// Returns [`AnalyzerVersionError::Zero`] when `value` is zero.
    pub fn new(value: u32) -> Result<Self, AnalyzerVersionError> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or(AnalyzerVersionError::Zero)
    }

    /// Returns the positive integer version.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for AnalyzerVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

/// Validation errors for an [`AnalyzerVersion`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalyzerVersionError {
    /// Analyzer versions must not be zero.
    Zero,
}

impl fmt::Display for AnalyzerVersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("analyzer version must be non-zero"),
        }
    }
}

impl Error for AnalyzerVersionError {}

/// A finite confidence value in the inclusive range `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MomentConfidence(f32);

impl MomentConfidence {
    /// Lowest possible confidence.
    pub const ZERO: Self = Self(0.0);

    /// Highest possible confidence.
    pub const CERTAIN: Self = Self(1.0);

    /// Constructs a validated confidence value.
    ///
    /// Negative zero is canonicalized to positive zero.
    ///
    /// # Errors
    ///
    /// Returns [`MomentConfidenceError::NonFinite`] for NaN or infinity and
    /// [`MomentConfidenceError::OutOfRange`] for finite values outside
    /// `[0, 1]`.
    pub fn new(value: f32) -> Result<Self, MomentConfidenceError> {
        if !value.is_finite() {
            return Err(MomentConfidenceError::NonFinite);
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(MomentConfidenceError::OutOfRange);
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }

    /// Returns the finite value in `[0, 1]`.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for MomentConfidence {
    fn default() -> Self {
        Self::ZERO
    }
}

impl Eq for MomentConfidence {}

impl PartialOrd for MomentConfidence {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MomentConfidence {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl fmt::Display for MomentConfidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

/// Validation errors for [`MomentConfidence`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MomentConfidenceError {
    /// The supplied value was NaN or positive/negative infinity.
    NonFinite,
    /// The finite value was below zero or above one.
    OutOfRange,
}

impl fmt::Display for MomentConfidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite => formatter.write_str("moment confidence must be finite"),
            Self::OutOfRange => {
                formatter.write_str("moment confidence must be in the inclusive range [0, 1]")
            }
        }
    }
}

impl Error for MomentConfidenceError {}

/// A bounded opaque text or metadata payload attached to a moment.
///
/// Payloads are valid UTF-8 (as guaranteed by [`String`]), non-empty, and at
/// most [`MAX_MOMENT_PAYLOAD_BYTES`] bytes. Tab, line-feed, and carriage-return
/// are accepted; other Unicode control characters are rejected. The contents
/// are deliberately not interpreted as JSON or any other format.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MomentPayload(String);

impl MomentPayload {
    /// Validates and copies a payload.
    ///
    /// # Errors
    ///
    /// Returns [`MomentPayloadError`] when `value` is empty, too long, or
    /// contains a disallowed control character.
    pub fn new(value: &str) -> Result<Self, MomentPayloadError> {
        validate_moment_payload(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Returns the uninterpreted payload text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the payload's bounded UTF-8 byte length.
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.0.len()
    }
}

impl FromStr for MomentPayload {
    type Err = MomentPayloadError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for MomentPayload {
    type Error = MomentPayloadError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl AsRef<str> for MomentPayload {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for MomentPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Validation errors for a [`MomentPayload`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MomentPayloadError {
    /// The payload was empty; use `None` when a moment has no payload.
    Empty,
    /// The UTF-8 representation exceeded [`MAX_MOMENT_PAYLOAD_BYTES`].
    TooLong {
        /// Actual UTF-8 byte length.
        actual: usize,
    },
    /// The payload contained a control character other than tab, LF, or CR.
    DisallowedControl {
        /// Zero-based UTF-8 byte position of the character.
        index: usize,
        /// Disallowed character found at `index`.
        character: char,
    },
}

impl fmt::Display for MomentPayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("moment payload must not be empty; use no payload"),
            Self::TooLong { actual } => write!(
                formatter,
                "moment payload has {actual} bytes; maximum is {MAX_MOMENT_PAYLOAD_BYTES}"
            ),
            Self::DisallowedControl { index, character } => write!(
                formatter,
                "moment payload contains disallowed control U+{:04X} at byte index {index}",
                u32::from(*character)
            ),
        }
    }
}

impl Error for MomentPayloadError {}

fn validate_moment_payload(value: &str) -> Result<(), MomentPayloadError> {
    if value.is_empty() {
        return Err(MomentPayloadError::Empty);
    }
    if value.len() > MAX_MOMENT_PAYLOAD_BYTES {
        return Err(MomentPayloadError::TooLong {
            actual: value.len(),
        });
    }

    for (index, character) in value.char_indices() {
        if character.is_control() && !matches!(character, '\t' | '\n' | '\r') {
            return Err(MomentPayloadError::DisallowedControl { index, character });
        }
    }
    Ok(())
}

/// One immutable, validated result produced by an analyzer.
///
/// `span` is half-open and may be empty. An empty span represents a point
/// event at its start boundary. All owned text enters through bounded,
/// validated domain types.
///
/// The natural ordering is deterministic: content key, start time, end time,
/// kind, analyzer identity, analyzer version, descending confidence, then
/// payload (`None` before text, text lexicographically).
#[derive(Clone, Debug, PartialEq)]
pub struct MomentRecord {
    content_key: MediaContentKey,
    span: TimeSpan,
    kind: MomentKind,
    confidence: MomentConfidence,
    analyzer_identity: AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
    payload: Option<MomentPayload>,
}

impl MomentRecord {
    /// Constructs a record from values that have already been validated by
    /// their domain types.
    #[must_use]
    pub fn new(
        content_key: MediaContentKey,
        span: TimeSpan,
        kind: MomentKind,
        confidence: MomentConfidence,
        analyzer_identity: AnalyzerIdentity,
        analyzer_version: AnalyzerVersion,
        payload: Option<MomentPayload>,
    ) -> Self {
        Self {
            content_key,
            span,
            kind,
            confidence,
            analyzer_identity,
            analyzer_version,
            payload,
        }
    }

    /// Returns the media content key.
    #[must_use]
    pub const fn content_key(&self) -> MediaContentKey {
        self.content_key
    }

    /// Returns the half-open time span.
    #[must_use]
    pub const fn span(&self) -> TimeSpan {
        self.span
    }

    /// Returns the moment kind.
    #[must_use]
    pub const fn kind(&self) -> &MomentKind {
        &self.kind
    }

    /// Returns the confidence.
    #[must_use]
    pub const fn confidence(&self) -> MomentConfidence {
        self.confidence
    }

    /// Returns the analyzer/source identity.
    #[must_use]
    pub const fn analyzer_identity(&self) -> &AnalyzerIdentity {
        &self.analyzer_identity
    }

    /// Returns the analyzer result schema/version.
    #[must_use]
    pub const fn analyzer_version(&self) -> AnalyzerVersion {
        self.analyzer_version
    }

    /// Returns the optional uninterpreted payload.
    #[must_use]
    pub const fn payload(&self) -> Option<&MomentPayload> {
        self.payload.as_ref()
    }
}

impl Eq for MomentRecord {}

impl PartialOrd for MomentRecord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MomentRecord {
    fn cmp(&self, other: &Self) -> Ordering {
        self.content_key
            .cmp(&other.content_key)
            .then_with(|| cmp_valid_time(self.span.start_seconds(), other.span.start_seconds()))
            .then_with(|| cmp_valid_time(self.span.end_seconds(), other.span.end_seconds()))
            .then_with(|| self.kind.cmp(&other.kind))
            .then_with(|| self.analyzer_identity.cmp(&other.analyzer_identity))
            .then_with(|| self.analyzer_version.cmp(&other.analyzer_version))
            .then_with(|| other.confidence.cmp(&self.confidence))
            .then_with(|| self.payload.cmp(&other.payload))
    }
}

fn cmp_valid_time(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right)
        .expect("TimeSpan guarantees finite boundaries")
}

/// A validated in-memory moments query.
///
/// The content key is always required. Kind matching is exact. Minimum
/// confidence is inclusive. With a non-empty query range `[query_start,
/// query_end)`:
///
/// - a non-empty record `[record_start, record_end)` matches when the two
///   half-open spans overlap (`record_start < query_end` and
///   `record_end > query_start`);
/// - a point record `[point, point)` matches when
///   `query_start <= point < query_end`.
///
/// Consequently, a span ending at the query start does not match, a span
/// starting at the query end does not match, a point at the query start does
/// match, and a point at the query end does not. An empty query range matches
/// no records.
#[derive(Clone, Debug, PartialEq)]
pub struct MomentQuery {
    content_key: MediaContentKey,
    kind: Option<MomentKind>,
    time_range: Option<TimeSpan>,
    minimum_confidence: MomentConfidence,
}

impl MomentQuery {
    /// Constructs an unbounded query for one media content key.
    ///
    /// The initial query accepts every kind, every time, and confidence zero
    /// or greater.
    #[must_use]
    pub const fn new(content_key: MediaContentKey) -> Self {
        Self {
            content_key,
            kind: None,
            time_range: None,
            minimum_confidence: MomentConfidence::ZERO,
        }
    }

    /// Adds an exact kind filter.
    #[must_use]
    pub fn with_kind(mut self, kind: MomentKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Adds a half-open time-range filter.
    #[must_use]
    pub const fn with_time_range(mut self, time_range: TimeSpan) -> Self {
        self.time_range = Some(time_range);
        self
    }

    /// Adds an inclusive minimum-confidence filter.
    #[must_use]
    pub const fn with_minimum_confidence(mut self, minimum_confidence: MomentConfidence) -> Self {
        self.minimum_confidence = minimum_confidence;
        self
    }

    /// Returns the required media content key.
    #[must_use]
    pub const fn content_key(&self) -> MediaContentKey {
        self.content_key
    }

    /// Returns the optional exact kind filter.
    #[must_use]
    pub const fn kind(&self) -> Option<&MomentKind> {
        self.kind.as_ref()
    }

    /// Returns the optional half-open time range.
    #[must_use]
    pub const fn time_range(&self) -> Option<TimeSpan> {
        self.time_range
    }

    /// Returns the inclusive minimum confidence.
    #[must_use]
    pub const fn minimum_confidence(&self) -> MomentConfidence {
        self.minimum_confidence
    }

    /// Returns whether a record satisfies every query filter.
    ///
    /// Time matching follows the point-versus-span boundary semantics
    /// documented on [`MomentQuery`].
    #[must_use]
    pub fn matches(&self, record: &MomentRecord) -> bool {
        record.content_key == self.content_key
            && self.kind.as_ref().is_none_or(|kind| record.kind == *kind)
            && record.confidence >= self.minimum_confidence
            && self
                .time_range
                .is_none_or(|range| span_matches_range(record.span, range))
    }
}

impl Eq for MomentQuery {}

fn span_matches_range(record: TimeSpan, query: TimeSpan) -> bool {
    if query.is_empty() {
        return false;
    }

    if record.is_empty() {
        query.start_seconds() <= record.start_seconds()
            && record.start_seconds() < query.end_seconds()
    } else {
        record.start_seconds() < query.end_seconds() && record.end_seconds() > query.start_seconds()
    }
}

/// Filters borrowed records and returns them in canonical value order.
///
/// Results are independent of input order. Within the required content key,
/// ordering is start time, end time, kind, analyzer identity, analyzer version,
/// descending confidence, and payload (`None` before lexicographic text).
///
/// For `n` input records and `m` matches, complexity is `O(n + m log m)` time
/// and `O(m)` output space. Filtering is one linear pass; no pairwise scan is
/// performed.
#[must_use]
pub fn filter_and_sort_moments<'a>(
    records: &'a [MomentRecord],
    query: &MomentQuery,
) -> Vec<&'a MomentRecord> {
    let mut matches: Vec<_> = records
        .iter()
        .filter(|record| query.matches(record))
        .collect();
    matches.sort_unstable();
    matches
}

/// The replacement scope for one analyzer's complete result set.
///
/// Storage adapters should use the full tuple `(content key, analyzer
/// identity, analyzer version)` as the delete-and-insert scope. Results from
/// another analyzer or version are outside this key and must remain untouched.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MomentBatchKey {
    content_key: MediaContentKey,
    analyzer_identity: AnalyzerIdentity,
    analyzer_version: AnalyzerVersion,
}

impl MomentBatchKey {
    /// Constructs a replacement key from validated components.
    #[must_use]
    pub const fn new(
        content_key: MediaContentKey,
        analyzer_identity: AnalyzerIdentity,
        analyzer_version: AnalyzerVersion,
    ) -> Self {
        Self {
            content_key,
            analyzer_identity,
            analyzer_version,
        }
    }

    /// Returns the media content key.
    #[must_use]
    pub const fn content_key(&self) -> MediaContentKey {
        self.content_key
    }

    /// Returns the analyzer/source identity.
    #[must_use]
    pub const fn analyzer_identity(&self) -> &AnalyzerIdentity {
        &self.analyzer_identity
    }

    /// Returns the analyzer result schema/version.
    #[must_use]
    pub const fn analyzer_version(&self) -> AnalyzerVersion {
        self.analyzer_version
    }
}

/// A validated complete replacement batch for one [`MomentBatchKey`].
///
/// Construction rejects records outside the key and duplicate exact records,
/// then stores accepted records in [`MomentRecord`]'s canonical order. An empty
/// batch is valid and means the analyzer completed with no results, allowing a
/// storage transaction to remove its previous rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MomentReplacementBatch {
    key: MomentBatchKey,
    records: Vec<MomentRecord>,
}

impl MomentReplacementBatch {
    /// Validates and canonically orders a complete analyzer result set.
    ///
    /// # Complexity
    ///
    /// For `n` records, construction takes `O(n log n)` time and `O(n)` space
    /// for the owned batch. Validation itself is one linear pass; sorting also
    /// enables adjacent duplicate detection without an `O(n²)` scan.
    ///
    /// # Errors
    ///
    /// Returns [`MomentBatchError`] for the first record whose content key,
    /// analyzer identity, or analyzer version differs from `key`, or for a
    /// duplicate exact-record pair. Duplicate errors report original input
    /// positions.
    pub fn new(key: MomentBatchKey, records: Vec<MomentRecord>) -> Result<Self, MomentBatchError> {
        for (record_index, record) in records.iter().enumerate() {
            if record.content_key != key.content_key {
                return Err(MomentBatchError::ContentKeyMismatch { record_index });
            }
            if record.analyzer_identity != key.analyzer_identity {
                return Err(MomentBatchError::AnalyzerIdentityMismatch { record_index });
            }
            if record.analyzer_version != key.analyzer_version {
                return Err(MomentBatchError::AnalyzerVersionMismatch { record_index });
            }
        }

        let mut indexed: Vec<_> = records.into_iter().enumerate().collect();
        indexed.sort_unstable_by(|(left_index, left_record), (right_index, right_record)| {
            left_record
                .cmp(right_record)
                .then_with(|| left_index.cmp(right_index))
        });

        for pair in indexed.windows(2) {
            if pair[0].1 == pair[1].1 {
                return Err(MomentBatchError::DuplicateRecord {
                    first_index: pair[0].0,
                    duplicate_index: pair[1].0,
                });
            }
        }

        Ok(Self {
            key,
            records: indexed.into_iter().map(|(_, record)| record).collect(),
        })
    }

    /// Returns the atomic replacement scope.
    #[must_use]
    pub const fn key(&self) -> &MomentBatchKey {
        &self.key
    }

    /// Returns the canonically ordered complete result set.
    #[must_use]
    pub fn records(&self) -> &[MomentRecord] {
        &self.records
    }

    /// Returns the number of records in the complete result set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns whether the complete result set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Consumes the batch into its replacement key and canonical records.
    #[must_use]
    pub fn into_parts(self) -> (MomentBatchKey, Vec<MomentRecord>) {
        (self.key, self.records)
    }
}

/// Errors produced while validating a [`MomentReplacementBatch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MomentBatchError {
    /// A record belonged to another media content key.
    ContentKeyMismatch {
        /// Zero-based position in the supplied records.
        record_index: usize,
    },
    /// A record belonged to another analyzer identity.
    AnalyzerIdentityMismatch {
        /// Zero-based position in the supplied records.
        record_index: usize,
    },
    /// A record belonged to another analyzer version.
    AnalyzerVersionMismatch {
        /// Zero-based position in the supplied records.
        record_index: usize,
    },
    /// Two supplied records were exactly equal in every domain field.
    DuplicateRecord {
        /// Original position of the first equal record.
        first_index: usize,
        /// Original position of the repeated equal record.
        duplicate_index: usize,
    },
}

impl fmt::Display for MomentBatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContentKeyMismatch { record_index } => write!(
                formatter,
                "moment at index {record_index} has a different media content key"
            ),
            Self::AnalyzerIdentityMismatch { record_index } => write!(
                formatter,
                "moment at index {record_index} has a different analyzer identity"
            ),
            Self::AnalyzerVersionMismatch { record_index } => write!(
                formatter,
                "moment at index {record_index} has a different analyzer version"
            ),
            Self::DuplicateRecord {
                first_index,
                duplicate_index,
            } => write!(
                formatter,
                "moment at index {duplicate_index} duplicates moment at index {first_index}"
            ),
        }
    }
}

impl Error for MomentBatchError {}
