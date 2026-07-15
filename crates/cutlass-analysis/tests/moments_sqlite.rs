#![cfg(feature = "sqlite")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use cutlass_analysis::TimeSpan;
use cutlass_analysis::moments::{
    AnalyzerIdentity, AnalyzerVersion, DEFAULT_MOMENTS_QUERY_LIMIT, MAX_MOMENT_PAYLOAD_BYTES,
    MAX_MOMENTS_BUSY_TIMEOUT, MAX_MOMENTS_QUERY_LIMIT, MEDIA_CONTENT_KEY_BYTES,
    MOMENTS_INDEX_DATABASE_FILENAME, MOMENTS_INDEX_SCHEMA_VERSION, MediaContentKey, MomentBatchKey,
    MomentConfidence, MomentKind, MomentPayload, MomentQuery, MomentRecord, MomentReplacementBatch,
    MomentsIndex, MomentsIndexConfig, MomentsIndexError, filter_and_sort_moments,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

fn content_key(marker: u8) -> MediaContentKey {
    MediaContentKey::from_bytes([marker; MEDIA_CONTENT_KEY_BYTES])
}

fn analyzer(identity: &str) -> AnalyzerIdentity {
    AnalyzerIdentity::new(identity).expect("test analyzer identity is valid")
}

fn version(value: u32) -> AnalyzerVersion {
    AnalyzerVersion::new(value).expect("test analyzer version is valid")
}

fn batch_key(
    key: MediaContentKey,
    analyzer_identity: &str,
    analyzer_version: u32,
) -> MomentBatchKey {
    MomentBatchKey::new(key, analyzer(analyzer_identity), version(analyzer_version))
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

fn replacement(key: MomentBatchKey, records: Vec<MomentRecord>) -> MomentReplacementBatch {
    MomentReplacementBatch::new(key, records).expect("test replacement batch is valid")
}

fn replace_grouped(index: &MomentsIndex, records: &[MomentRecord]) {
    let mut groups: BTreeMap<MomentBatchKey, Vec<MomentRecord>> = BTreeMap::new();
    for record in records {
        let key = MomentBatchKey::new(
            record.content_key(),
            record.analyzer_identity().clone(),
            record.analyzer_version(),
        );
        groups.entry(key).or_default().push(record.clone());
    }
    for (key, records) in groups {
        index
            .replace_batch(&replacement(key, records))
            .expect("grouped test batch persists");
    }
}

fn query_records(index: &MomentsIndex, query: &MomentQuery) -> Vec<MomentRecord> {
    index
        .query_with_limit(query, MAX_MOMENTS_QUERY_LIMIT)
        .expect("test query succeeds")
        .into_records()
}

fn expected_records(records: &[MomentRecord], query: &MomentQuery) -> Vec<MomentRecord> {
    filter_and_sort_moments(records, query)
        .into_iter()
        .cloned()
        .collect()
}

fn payload_label(record: &MomentRecord) -> &str {
    record.payload().map_or("<none>", MomentPayload::as_str)
}

fn database_path(directory: &TempDir) -> PathBuf {
    directory.path().join("nested").join("moments.sqlite3")
}

fn raw_connection(path: &Path) -> Connection {
    Connection::open(path).expect("raw test SQLite connection opens")
}

#[test]
fn fresh_cache_root_creates_schema_and_version_one() {
    let directory = TempDir::new().expect("temporary directory");
    let cache_root = directory.path().join("new").join("analysis-cache");
    let expected_path = cache_root.join(MOMENTS_INDEX_DATABASE_FILENAME);

    let index = MomentsIndex::open_in_cache_root(&cache_root).expect("fresh index opens");
    assert_eq!(index.path(), expected_path);
    assert_eq!(
        index.config().default_query_limit(),
        DEFAULT_MOMENTS_QUERY_LIMIT
    );
    drop(index);

    let raw = raw_connection(&expected_path);
    let user_version: u32 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("user_version is readable");
    assert_eq!(user_version, MOMENTS_INDEX_SCHEMA_VERSION);

    let schemas: BTreeMap<String, String> = {
        let mut statement = raw
            .prepare(
                "
                    SELECT name, sql
                    FROM sqlite_schema
                    WHERE name IN (
                        'moment_batches',
                        'moments',
                        'moments_query_order'
                    )
                    ORDER BY name
                ",
            )
            .expect("schema query prepares");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("schema query runs")
            .collect::<Result<_, _>>()
            .expect("schema rows decode")
    };
    assert_eq!(schemas.len(), 3);
    assert!(schemas["moment_batches"].contains("WITHOUT ROWID"));
    assert!(schemas["moments"].contains("WITHOUT ROWID"));
    assert!(schemas["moments"].contains("payload_present INTEGER NOT NULL"));
    assert!(
        schemas["moments"].contains("length(CAST(payload_text AS BLOB))"),
        "payload bounds must count UTF-8 bytes"
    );

    let journal_mode: String = raw
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal mode is readable");
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
}

#[test]
fn concurrent_fresh_opens_converge_on_one_schema() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 4;

    let directory = TempDir::new().expect("temporary directory");
    for round in 0..ROUNDS {
        let path = Arc::new(
            directory
                .path()
                .join(format!("concurrent-open-{round}.sqlite3")),
        );
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);

        for _ in 0..THREADS {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                MomentsIndex::open(path.as_ref())
            }));
        }

        for handle in handles {
            handle
                .join()
                .expect("open worker does not panic")
                .expect("concurrent fresh open converges on the initialized schema");
        }

        let reopened = MomentsIndex::open(path.as_ref()).expect("initialized index reopens");
        assert!(reopened.query(&MomentQuery::new(content_key(0))).is_ok());
    }
}

#[test]
fn persisted_records_and_f32_confidence_survive_reopen() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    let key = content_key(1);
    let batch_key = batch_key(key, "cutlass/reopen", 7);
    let confidence = 0.123_456_79_f32;
    let saved = record(
        key,
        1.25,
        2.5,
        "vision_moment",
        confidence,
        "cutlass/reopen",
        7,
        Some("persisted"),
    );

    {
        let index = MomentsIndex::open(&path).expect("index opens");
        index
            .replace_batch(&replacement(batch_key.clone(), vec![saved.clone()]))
            .expect("record persists");
    }

    let reopened = MomentsIndex::open(&path).expect("index reopens");
    assert!(reopened.batch_exists(&batch_key).expect("marker query"));
    let records = query_records(&reopened, &MomentQuery::new(key));
    assert_eq!(records, [saved]);
    assert_eq!(
        records[0].confidence().get().to_bits(),
        confidence.to_bits()
    );
}

#[test]
fn empty_replacement_retains_marker_and_delete_removes_it() {
    let directory = TempDir::new().expect("temporary directory");
    let index = MomentsIndex::open(database_path(&directory)).expect("index opens");
    let key = content_key(2);
    let batch_key = batch_key(key, "cutlass/empty", 1);

    index
        .replace_batch(&replacement(batch_key.clone(), Vec::new()))
        .expect("empty replacement persists");
    assert!(index.batch_exists(&batch_key).expect("marker query"));
    assert!(query_records(&index, &MomentQuery::new(key)).is_empty());

    assert!(index.delete_batch(&batch_key).expect("batch deletes"));
    assert!(!index.batch_exists(&batch_key).expect("marker query"));
    assert!(
        !index
            .delete_batch(&batch_key)
            .expect("second delete is a no-op")
    );
}

#[test]
fn analyzer_and_version_batches_replace_and_delete_in_isolation() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    let index = MomentsIndex::open(&path).expect("index opens");
    let key = content_key(3);
    let a1 = batch_key(key, "source/a", 1);
    let a2 = batch_key(key, "source/a", 2);
    let b1 = batch_key(key, "source/b", 1);

    for (batch, label) in [(&a1, "a1-old"), (&a2, "a2"), (&b1, "b1")] {
        index
            .replace_batch(&replacement(
                batch.clone(),
                vec![record(
                    key,
                    0.0,
                    0.0,
                    "beat",
                    0.5,
                    batch.analyzer_identity().as_str(),
                    batch.analyzer_version().get(),
                    Some(label),
                )],
            ))
            .expect("isolated batch persists");
    }

    let a1_new = record(key, 1.0, 1.0, "beat", 0.75, "source/a", 1, Some("a1-new"));
    index
        .replace_batch(&replacement(a1.clone(), vec![a1_new]))
        .expect("one batch replaces");

    let queried = query_records(&index, &MomentQuery::new(key));
    let labels: Vec<_> = queried.iter().map(payload_label).collect();
    assert_eq!(labels, ["a2", "b1", "a1-new"]);
    assert!(index.batch_exists(&a1).expect("a1 marker"));
    assert!(index.batch_exists(&a2).expect("a2 marker"));
    assert!(index.batch_exists(&b1).expect("b1 marker"));

    assert!(index.delete_batch(&a2).expect("a2 deletes"));
    let queried = query_records(&index, &MomentQuery::new(key));
    let labels: Vec<_> = queried.iter().map(payload_label).collect();
    assert_eq!(labels, ["b1", "a1-new"]);
    assert!(index.batch_exists(&a1).expect("a1 remains"));
    assert!(!index.batch_exists(&a2).expect("a2 is gone"));
    assert!(index.batch_exists(&b1).expect("b1 remains"));

    let raw = raw_connection(&path);
    let deleted_children: i64 = raw
        .query_row(
            "
                SELECT COUNT(*)
                FROM moments
                WHERE content_key = ?1
                  AND analyzer_identity = 'source/a'
                  AND analyzer_version = 2
            ",
            params![&key.as_bytes()[..]],
            |row| row.get(0),
        )
        .expect("child count query");
    assert_eq!(deleted_children, 0, "foreign-key cascade removes rows");
}

#[test]
fn sqlite_time_matching_has_exact_in_memory_boundary_parity() {
    let directory = TempDir::new().expect("temporary directory");
    let index = MomentsIndex::open(database_path(&directory)).expect("index opens");
    let key = content_key(4);
    let records = vec![
        record(
            key,
            0.0,
            1.0,
            "silence",
            1.0,
            "cutlass/time",
            1,
            Some("span-ending-at-start"),
        ),
        record(
            key,
            0.5,
            1.25,
            "silence",
            1.0,
            "cutlass/time",
            1,
            Some("span-overlapping-start"),
        ),
        record(
            key,
            1.0,
            1.0,
            "beat",
            1.0,
            "cutlass/time",
            1,
            Some("point-at-start"),
        ),
        record(
            key,
            1.0,
            1.4,
            "silence",
            1.0,
            "cutlass/time",
            1,
            Some("span-at-start"),
        ),
        record(
            key,
            1.5,
            1.5,
            "beat",
            1.0,
            "cutlass/time",
            1,
            Some("point-inside"),
        ),
        record(
            key,
            1.75,
            2.0,
            "silence",
            1.0,
            "cutlass/time",
            1,
            Some("span-ending-at-end"),
        ),
        record(
            key,
            2.0,
            2.0,
            "beat",
            1.0,
            "cutlass/time",
            1,
            Some("point-at-end"),
        ),
        record(
            key,
            2.0,
            2.5,
            "silence",
            1.0,
            "cutlass/time",
            1,
            Some("span-at-end"),
        ),
        record(
            key,
            0.0,
            3.0,
            "silence",
            1.0,
            "cutlass/time",
            1,
            Some("encloses-empty-query-point"),
        ),
    ];
    replace_grouped(&index, &records);

    let query =
        MomentQuery::new(key).with_time_range(TimeSpan::new(1.0, 2.0).expect("valid test range"));
    assert_eq!(
        query_records(&index, &query),
        expected_records(&records, &query)
    );

    let empty_query =
        MomentQuery::new(key).with_time_range(TimeSpan::new(1.0, 1.0).expect("valid empty range"));
    assert!(
        query_records(&index, &empty_query).is_empty(),
        "an empty query must not match even an enclosing span"
    );
    assert!(expected_records(&records, &empty_query).is_empty());
}

#[test]
fn kind_and_inclusive_confidence_filters_match_in_memory() {
    let directory = TempDir::new().expect("temporary directory");
    let index = MomentsIndex::open(database_path(&directory)).expect("index opens");
    let key = content_key(5);
    let other_key = content_key(6);
    let records = vec![
        record(
            key,
            0.0,
            0.0,
            "beat",
            0.5,
            "cutlass/filter",
            1,
            Some("inclusive"),
        ),
        record(
            key,
            1.0,
            1.0,
            "beat",
            0.49,
            "cutlass/filter",
            1,
            Some("below"),
        ),
        record(
            key,
            2.0,
            3.0,
            "silence",
            0.9,
            "cutlass/filter",
            1,
            Some("wrong-kind"),
        ),
        record(
            other_key,
            0.0,
            0.0,
            "beat",
            1.0,
            "cutlass/filter",
            1,
            Some("wrong-content"),
        ),
    ];
    replace_grouped(&index, &records);

    let query = MomentQuery::new(key)
        .with_kind(MomentKind::BEAT)
        .with_minimum_confidence(MomentConfidence::new(0.5).expect("valid threshold"));
    assert_eq!(
        query_records(&index, &query),
        expected_records(&records, &query)
    );
    assert_eq!(
        payload_label(&query_records(&index, &query)[0]),
        "inclusive"
    );
}

#[test]
fn sqlite_order_uses_every_moment_record_tie_breaker() {
    let directory = TempDir::new().expect("temporary directory");
    let index = MomentsIndex::open(database_path(&directory)).expect("index opens");
    let key = content_key(7);
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
    replace_grouped(&index, &records);
    let query = MomentQuery::new(key);

    let actual = query_records(&index, &query);
    assert_eq!(actual, expected_records(&records, &query));
    let labels: Vec<_> = actual.iter().map(payload_label).collect();
    assert_eq!(
        labels,
        [
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
        ]
    );
}

#[test]
fn truncation_is_exact_for_n_and_n_plus_one_rows() {
    let directory = TempDir::new().expect("temporary directory");
    let config =
        MomentsIndexConfig::new(2, 3, Duration::from_millis(250)).expect("test config is valid");
    let index =
        MomentsIndex::open_with_config(database_path(&directory), config).expect("index opens");
    let two_key = content_key(8);
    let three_key = content_key(9);
    let two = vec![
        record(two_key, 0.0, 0.0, "beat", 1.0, "source/limit", 1, Some("0")),
        record(two_key, 1.0, 1.0, "beat", 1.0, "source/limit", 1, Some("1")),
    ];
    let three = vec![
        record(
            three_key,
            0.0,
            0.0,
            "beat",
            1.0,
            "source/limit",
            1,
            Some("0"),
        ),
        record(
            three_key,
            1.0,
            1.0,
            "beat",
            1.0,
            "source/limit",
            1,
            Some("1"),
        ),
        record(
            three_key,
            2.0,
            2.0,
            "beat",
            1.0,
            "source/limit",
            1,
            Some("2"),
        ),
    ];
    replace_grouped(&index, &two);
    replace_grouped(&index, &three);

    let exact = index
        .query(&MomentQuery::new(two_key))
        .expect("exact-N query succeeds");
    assert_eq!(exact.len(), 2);
    assert!(!exact.truncated());

    let extra = index
        .query(&MomentQuery::new(three_key))
        .expect("N+1 query succeeds");
    assert_eq!(extra.len(), 2);
    assert!(extra.truncated());
    assert_eq!(
        extra
            .records()
            .iter()
            .map(payload_label)
            .collect::<Vec<_>>(),
        ["0", "1"]
    );

    let all = index
        .query_with_limit(&MomentQuery::new(three_key), 3)
        .expect("configured hard maximum is accepted");
    assert_eq!(all.len(), 3);
    assert!(!all.truncated());
}

#[test]
fn invalid_config_and_query_limits_are_rejected() {
    assert!(matches!(
        MomentsIndexConfig::new(0, 1, Duration::ZERO),
        Err(MomentsIndexError::InvalidConfig {
            field: "default_query_limit",
            ..
        })
    ));
    assert!(matches!(
        MomentsIndexConfig::new(2, 1, Duration::ZERO),
        Err(MomentsIndexError::InvalidConfig {
            field: "default_query_limit",
            ..
        })
    ));
    assert!(matches!(
        MomentsIndexConfig::new(1, MAX_MOMENTS_QUERY_LIMIT + 1, Duration::ZERO),
        Err(MomentsIndexError::InvalidConfig {
            field: "maximum_query_limit",
            ..
        })
    ));
    assert!(matches!(
        MomentsIndexConfig::new(1, 1, MAX_MOMENTS_BUSY_TIMEOUT + Duration::from_millis(1)),
        Err(MomentsIndexError::InvalidConfig {
            field: "busy_timeout",
            ..
        })
    ));

    let directory = TempDir::new().expect("temporary directory");
    let config = MomentsIndexConfig::new(1, 2, Duration::ZERO).expect("test config is valid");
    let index =
        MomentsIndex::open_with_config(database_path(&directory), config).expect("index opens");
    let query = MomentQuery::new(content_key(10));
    for requested in [0, 3, usize::MAX] {
        assert!(matches!(
            index.query_with_limit(&query, requested),
            Err(MomentsIndexError::InvalidQueryLimit {
                requested: actual,
                maximum: 2,
            }) if actual == requested
        ));
    }
}

#[test]
fn unsupported_future_schema_is_rejected_without_deleting_file() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    std::fs::create_dir_all(path.parent().expect("database parent")).expect("parent creates");
    let raw = raw_connection(&path);
    raw.pragma_update(
        None,
        "user_version",
        i64::from(MOMENTS_INDEX_SCHEMA_VERSION) + 1,
    )
    .expect("future version is set");
    drop(raw);

    let error = MomentsIndex::open(&path)
        .err()
        .expect("future schema must be rejected");
    assert!(matches!(
        error,
        MomentsIndexError::UnsupportedSchema {
            found,
            supported: MOMENTS_INDEX_SCHEMA_VERSION,
        } if found == i64::from(MOMENTS_INDEX_SCHEMA_VERSION) + 1
    ));
    assert!(path.exists(), "unsupported database is never auto-deleted");
    let raw = raw_connection(&path);
    let version: i64 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("version remains readable");
    assert_eq!(version, i64::from(MOMENTS_INDEX_SCHEMA_VERSION) + 1);
}

#[test]
fn claimed_current_version_without_schema_is_rejected() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    std::fs::create_dir_all(path.parent().expect("database parent")).expect("parent creates");
    let raw = raw_connection(&path);
    raw.pragma_update(
        None,
        "user_version",
        i64::from(MOMENTS_INDEX_SCHEMA_VERSION),
    )
    .expect("claimed version is set");
    drop(raw);

    let error = MomentsIndex::open(&path)
        .err()
        .expect("missing schema must be rejected");
    assert!(matches!(error, MomentsIndexError::SchemaMismatch { .. }));
    assert!(path.exists(), "invalid database is never auto-deleted");
}

#[test]
fn malformed_database_is_typed_and_never_replaced() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    std::fs::create_dir_all(path.parent().expect("database parent")).expect("parent creates");
    let original = b"this is not a SQLite database";
    std::fs::write(&path, original).expect("malformed database fixture writes");

    let error = MomentsIndex::open(&path)
        .err()
        .expect("malformed database must be rejected");
    assert!(matches!(error, MomentsIndexError::DatabaseCorrupt { .. }));
    assert_eq!(
        std::fs::read(&path).expect("malformed database remains readable"),
        original,
        "opening corruption must not replace or delete the file"
    );
}

#[test]
fn external_write_contention_maps_to_database_locked() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    let config =
        MomentsIndexConfig::new(1, 1, Duration::ZERO).expect("zero-timeout config is valid");
    let index = MomentsIndex::open_with_config(&path, config).expect("index opens");
    let key = content_key(16);
    let batch_key = batch_key(key, "source/locked", 1);
    index
        .replace_batch(&replacement(batch_key.clone(), Vec::new()))
        .expect("prior marker persists");

    let raw = raw_connection(&path);
    raw.execute_batch("BEGIN IMMEDIATE")
        .expect("raw connection acquires write lock");
    let error = index
        .replace_batch(&replacement(batch_key.clone(), Vec::new()))
        .expect_err("contended replacement must fail");
    assert!(matches!(error, MomentsIndexError::DatabaseLocked { .. }));
    raw.execute_batch("ROLLBACK")
        .expect("raw write lock releases");
    assert!(
        index
            .batch_exists(&batch_key)
            .expect("prior marker remains")
    );
}

#[test]
fn schema_checks_storage_types_ascii_bounds_and_payload_bytes() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    let index = MomentsIndex::open(&path).expect("index opens");
    let key = content_key(11);
    let marker = batch_key(key, "source/checks", 1);
    index
        .replace_batch(&replacement(marker, Vec::new()))
        .expect("parent marker persists");

    let raw = raw_connection(&path);
    raw.pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys enable");

    let invalid_identity = raw.execute(
        "
            INSERT INTO moment_batches (
                content_key,
                analyzer_identity,
                analyzer_version
            ) VALUES (?1, 'Source/Uppercase', 1)
        ",
        params![&content_key(12).as_bytes()[..]],
    );
    assert!(invalid_identity.is_err());

    let invalid_version = raw.execute(
        "
            INSERT INTO moment_batches (
                content_key,
                analyzer_identity,
                analyzer_version
            ) VALUES (?1, 'source/version', 0)
        ",
        params![&content_key(13).as_bytes()[..]],
    );
    assert!(invalid_version.is_err());

    let invalid_time = raw.execute(
        "
            INSERT INTO moments (
                content_key,
                analyzer_identity,
                analyzer_version,
                start_seconds,
                end_seconds,
                kind,
                confidence,
                payload_present,
                payload_text
            ) VALUES (?1, 'source/checks', 1, ?2, ?2, 'beat', 1.0, 0, '')
        ",
        params![&key.as_bytes()[..], f64::INFINITY],
    );
    assert!(invalid_time.is_err());

    let maximum_multibyte_payload = "é".repeat(MAX_MOMENT_PAYLOAD_BYTES / "é".len());
    assert_eq!(maximum_multibyte_payload.len(), MAX_MOMENT_PAYLOAD_BYTES);
    raw.execute(
        "
            INSERT INTO moments (
                content_key,
                analyzer_identity,
                analyzer_version,
                start_seconds,
                end_seconds,
                kind,
                confidence,
                payload_present,
                payload_text
            ) VALUES (?1, 'source/checks', 1, 1.0, 1.0, 'beat', 1.0, 1, ?2)
        ",
        params![&key.as_bytes()[..], maximum_multibyte_payload],
    )
    .expect("payload at the UTF-8 byte bound is accepted");

    let oversized_multibyte_payload = "é".repeat(MAX_MOMENT_PAYLOAD_BYTES / "é".len() + 1);
    let oversized = raw.execute(
        "
            INSERT INTO moments (
                content_key,
                analyzer_identity,
                analyzer_version,
                start_seconds,
                end_seconds,
                kind,
                confidence,
                payload_present,
                payload_text
            ) VALUES (?1, 'source/checks', 1, 2.0, 2.0, 'beat', 1.0, 1, ?2)
        ",
        params![&key.as_bytes()[..], oversized_multibyte_payload],
    );
    assert!(oversized.is_err());
}

#[test]
fn corrupt_rows_fail_closed_with_bounded_errors() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    let index = MomentsIndex::open(&path).expect("index opens");
    let key = content_key(14);
    let marker = batch_key(key, "source/corrupt", 1);
    index
        .replace_batch(&replacement(marker, Vec::new()))
        .expect("parent marker persists");

    let raw = raw_connection(&path);
    raw.pragma_update(None, "ignore_check_constraints", "ON")
        .expect("test corruption injection enables");
    raw.execute(
        "
            INSERT INTO moments (
                content_key,
                analyzer_identity,
                analyzer_version,
                start_seconds,
                end_seconds,
                kind,
                confidence,
                payload_present,
                payload_text
            ) VALUES (?1, 'source/corrupt', 1, 'not-a-time', 1.0, 'beat', 1.0, 0, '')
        ",
        params![&key.as_bytes()[..]],
    )
    .expect("type-invalid row is injected");

    let error = index
        .query(&MomentQuery::new(key))
        .expect_err("type-invalid row must fail closed");
    assert!(matches!(
        error,
        MomentsIndexError::CorruptRow {
            row_index: 0,
            field: "start_seconds",
            ..
        }
    ));

    raw.execute("DELETE FROM moments", [])
        .expect("first corrupt row deletes");
    let hostile_payload = format!("secret-prefix-{}", "x".repeat(100_000));
    raw.execute(
        "
            INSERT INTO moments (
                content_key,
                analyzer_identity,
                analyzer_version,
                start_seconds,
                end_seconds,
                kind,
                confidence,
                payload_present,
                payload_text
            ) VALUES (?1, 'source/corrupt', 1, 1.0, 1.0, 'beat', 1.0, 1, ?2)
        ",
        params![&key.as_bytes()[..], hostile_payload],
    )
    .expect("domain-invalid payload is injected");

    let error = index
        .query(&MomentQuery::new(key))
        .expect_err("oversized payload must fail closed");
    assert!(matches!(
        error,
        MomentsIndexError::CorruptRow {
            row_index: 0,
            field: "payload_text",
            ..
        }
    ));
    let display = error.to_string();
    assert!(display.len() < 256);
    assert!(!display.contains("secret-prefix"));
}

#[test]
fn failed_mid_insert_replacement_rolls_back_prior_complete_batch() {
    let directory = TempDir::new().expect("temporary directory");
    let path = database_path(&directory);
    let index = MomentsIndex::open(&path).expect("index opens");
    let key = content_key(15);
    let batch_key = batch_key(key, "source/rollback", 1);
    let old = record(
        key,
        0.0,
        0.0,
        "beat",
        1.0,
        "source/rollback",
        1,
        Some("old-complete"),
    );
    index
        .replace_batch(&replacement(batch_key.clone(), vec![old.clone()]))
        .expect("prior complete batch persists");

    let raw = raw_connection(&path);
    raw.execute_batch(
        "
            CREATE TRIGGER reject_test_moment
            BEFORE INSERT ON moments
            WHEN NEW.payload_text = 'abort'
            BEGIN
                SELECT RAISE(ABORT, 'injected test failure');
            END;
        ",
    )
    .expect("failure trigger installs");
    drop(raw);

    let replacement_records = vec![
        record(
            key,
            1.0,
            1.0,
            "beat",
            1.0,
            "source/rollback",
            1,
            Some("inserted-before-failure"),
        ),
        record(
            key,
            2.0,
            2.0,
            "beat",
            1.0,
            "source/rollback",
            1,
            Some("abort"),
        ),
    ];
    let error = index
        .replace_batch(&replacement(batch_key.clone(), replacement_records))
        .expect_err("trigger aborts replacement");
    assert!(matches!(
        error,
        MomentsIndexError::ConstraintViolation { .. }
    ));

    assert!(index.batch_exists(&batch_key).expect("old marker remains"));
    assert_eq!(
        query_records(&index, &MomentQuery::new(key)),
        [old],
        "delete and earlier inserts roll back with the failed transaction"
    );
}

#[test]
fn mutex_serializes_concurrent_replacements_and_queries() {
    const THREADS: usize = 6;
    const ITERATIONS: usize = 8;

    let directory = TempDir::new().expect("temporary directory");
    let index =
        Arc::new(MomentsIndex::open(database_path(&directory)).expect("shared index opens"));
    let barrier = Arc::new(Barrier::new(THREADS));
    let mut handles = Vec::new();

    for worker in 0..THREADS {
        let index = Arc::clone(&index);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let marker = u8::try_from(worker + 20).expect("worker marker fits u8");
            let key = content_key(marker);
            let analyzer_identity = format!("source/concurrent-{worker}");
            let batch_key = batch_key(key, &analyzer_identity, 1);
            barrier.wait();

            for iteration in 0..ITERATIONS {
                let start =
                    f64::from(u32::try_from(iteration).expect("test iteration count fits u32"));
                let record = record(
                    key,
                    start,
                    start,
                    "beat",
                    1.0,
                    &analyzer_identity,
                    1,
                    Some("current"),
                );
                index
                    .replace_batch(&replacement(batch_key.clone(), vec![record.clone()]))
                    .expect("concurrent replacement succeeds");
                let result = index
                    .query_with_limit(&MomentQuery::new(key), 1)
                    .expect("concurrent query succeeds");
                assert_eq!(result.records(), &[record]);
                assert!(!result.truncated());
            }
        }));
    }

    for handle in handles {
        handle.join().expect("worker does not panic");
    }
}
