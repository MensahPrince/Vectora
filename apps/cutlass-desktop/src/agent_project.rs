//! Strict, path-free project tools for the desktop agent.
//!
//! Draft paths remain an internal engine/storage detail. The model sees only
//! canonical draft identities and bounded display metadata.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use cutlass_ai::{HostToolSpec, ToolOutput, ToolTier};
use serde_json::{Map, Value, json};
use tracing::error;

use crate::preview_worker::{OpenProjectRpcResult, WorkerHandle};

const PROJECT_LIST_DRAFTS: &str = "project_list_drafts";
const PROJECT_SAVE: &str = "project_save";
const PROJECT_OPEN: &str = "project_open";
const TOOL_NAMES: [&str; 3] = [PROJECT_LIST_DRAFTS, PROJECT_SAVE, PROJECT_OPEN];

const DEFAULT_DRAFT_LIMIT: usize = 50;
const MAX_DRAFT_LIMIT: usize = 100;
const MIN_DRAFT_ID_CHARS: usize = 3;
/// A canonical id has at most 32 time hex digits, one dash, and 16 sequence
/// hex digits. Canonical ids are ASCII, so this is also their byte bound.
pub(crate) const MAX_DRAFT_ID_CHARS: usize = 32 + 1 + 16;
const MAX_DISPLAY_NAME_CHARS: usize = 128;
const MAX_OUTPUT_BYTES: usize = 128 * 1024;

pub(crate) fn specs() -> Vec<HostToolSpec> {
    vec![
        spec(
            PROJECT_LIST_DRAFTS,
            "List app-owned project drafts newest first using safe draft identities, display names, and modification times.",
            list_schema(),
            ToolTier::ReadOnly,
        ),
        spec(
            PROJECT_SAVE,
            "Save the current app-owned project and return its safe draft identity.",
            empty_object_schema(),
            ToolTier::Workspace,
        ),
        spec(
            PROJECT_OPEN,
            "Open an existing app-owned project by canonical draft identity. This replaces the current editor session.",
            open_schema(),
            ToolTier::System,
        ),
    ]
}

/// Whether a registered project tool can change live project/session state.
///
/// The read-only allowlist is explicit; unknown future project tools fail
/// closed as mutations until they are deliberately classified.
pub(crate) fn mutates_live_project(name: &str) -> bool {
    cutlass_ai::namespace(name) == "project" && name != PROJECT_LIST_DRAFTS
}

/// Strictly validate a project-tool request before any System approval UI is
/// published. Opening performs a read-only draft-store preflight here; the
/// dispatch path resolves the id again after approval.
pub(crate) fn validate_request(name: &str, arguments: &Value) -> Result<(), String> {
    match parse_request(name, arguments)? {
        Request::Open { draft_id } => resolve_open_draft(&draft_id).map(|_| ()),
        Request::ListDrafts { .. } | Request::Save => Ok(()),
    }
}

pub(crate) fn call(
    worker: Option<&WorkerHandle>,
    name: &str,
    arguments: &Value,
    cancel: &AtomicBool,
) -> Result<ToolOutput, String> {
    match parse_request(name, arguments)? {
        Request::ListDrafts { limit } => {
            if cancel.load(Ordering::Acquire) {
                return Err("cancelled before project_list_drafts could run".into());
            }
            let drafts = crate::drafts::list_checked().map_err(|internal| {
                error!(error = %internal, "agent draft listing failed");
                "project_list_drafts failed: the draft store could not be read".to_string()
            })?;
            if cancel.load(Ordering::Acquire) {
                return Err("cancelled while project_list_drafts was running".into());
            }
            list_drafts_output(drafts, limit)
        }
        Request::Save => save_project(worker, cancel),
        Request::Open { draft_id } => open_project(worker, &draft_id, cancel),
    }
}

fn open_project(
    worker: Option<&WorkerHandle>,
    draft_id: &str,
    cancel: &AtomicBool,
) -> Result<ToolOutput, String> {
    if cancel.load(Ordering::Acquire) {
        return Err("project_open was cancelled before it started".into());
    }
    // This is intentionally repeated after approval and immediately before
    // queueing the RPC. The preflight result is never trusted across the
    // approval wait.
    let path = resolve_open_draft(draft_id)?;
    let worker = worker.ok_or_else(|| {
        "project_open failed: the editor worker is unavailable; not started".to_string()
    })?;
    let opened = worker
        .open_project_rpc_with_cancel(path, cancel)
        .map_err(|internal| {
            error!(error = %internal, "agent project open failed");
            public_open_error(&internal)
        })?;
    open_output(draft_id, &opened)
}

fn resolve_open_draft(draft_id: &str) -> Result<std::path::PathBuf, String> {
    crate::drafts::resolve_draft_id(draft_id).map_err(|internal| {
        error!(error = %internal, "agent project-open draft resolution failed");
        public_draft_resolution_error(internal.kind())
    })
}

fn public_draft_resolution_error(kind: io::ErrorKind) -> String {
    match kind {
        io::ErrorKind::InvalidInput => {
            "project_open argument 'draft_id' must be a canonical app-owned draft ID".into()
        }
        io::ErrorKind::NotFound => {
            "project_open failed: the requested app-owned draft does not exist".into()
        }
        io::ErrorKind::PermissionDenied => {
            "project_open failed: the requested app-owned draft could not be safely resolved".into()
        }
        _ => "project_open failed: the draft store could not be read".into(),
    }
}

fn public_open_error(internal: &str) -> String {
    if internal.contains("outcome unknown")
        || internal.contains("outcome uncertain/partially committed")
    {
        "project_open was dispatched, but the editor did not confirm its result; outcome unknown: the project may have opened".into()
    } else if internal.contains("cancelled before worker claim; not started") {
        "project_open was cancelled before it started".into()
    } else if internal.contains("not started") {
        "project_open failed before the editor started it".into()
    } else {
        "project_open failed: the requested draft could not be opened".into()
    }
}

fn open_output(
    requested_draft_id: &str,
    opened: &OpenProjectRpcResult,
) -> Result<ToolOutput, String> {
    let actual_draft_id =
        crate::drafts::draft_id_from_project(&opened.path).map_err(|internal| {
            error!(
                project = %opened.path.display(),
                error = %internal,
                "agent project open returned a non-draft path"
            );
            "project_open opened a project, but could not confirm its app-owned draft identity; the current session was replaced".to_string()
        })?;
    if actual_draft_id != requested_draft_id {
        error!(
            project = %opened.path.display(),
            requested_draft_id,
            actual_draft_id,
            "agent project open acknowledged a different draft identity"
        );
        return Err(
            "project_open opened a project, but its draft identity did not match the request; the current session was replaced".into(),
        );
    }

    encode_output(
        PROJECT_OPEN,
        json!({
            "status": "ok",
            "draft_id": actual_draft_id,
            "project_name": bounded_display_name(&opened.project_name),
            "missing_media_count": opened.missing_media_count
        }),
    )
}

fn save_project(worker: Option<&WorkerHandle>, cancel: &AtomicBool) -> Result<ToolOutput, String> {
    let worker = worker
        .ok_or_else(|| "project_save failed: the editor worker is unavailable".to_string())?;
    let saved = worker
        .save_project_rpc_with_cancel(None, cancel)
        .map_err(|internal| {
            error!(error = %internal, "agent project save failed");
            public_save_error(&internal)
        })?;
    save_output(&saved.path, saved.dirty)
}

fn public_save_error(internal: &str) -> String {
    if internal.contains("cancelled before worker claim; not started") {
        "project_save was cancelled before it started".into()
    } else if internal.contains("no current project path is bound") {
        "project_save failed: no app-owned draft is bound".into()
    } else if internal.contains("outcome unknown") {
        "project_save was dispatched, but the editor did not confirm its result; outcome unknown"
            .into()
    } else if internal.contains("not started") {
        "project_save failed before the editor started it".into()
    } else {
        "project_save failed: the current draft could not be saved".into()
    }
}

fn save_output(path: &Path, dirty: bool) -> Result<ToolOutput, String> {
    if dirty {
        error!(
            project = %path.display(),
            "agent project save acknowledged an unexpectedly dirty project"
        );
        return Err("project_save could not confirm a clean saved state".into());
    }
    let draft_id = crate::drafts::draft_id_from_project(path).map_err(|internal| {
        error!(
            project = %path.display(),
            error = %internal,
            "agent project save returned a non-draft path"
        );
        "project_save could not confirm an app-owned draft identity after saving".to_string()
    })?;
    encode_output(
        PROJECT_SAVE,
        json!({
            "status": "ok",
            "draft_id": draft_id,
            "dirty": false
        }),
    )
}

fn list_drafts_output(
    mut drafts: Vec<crate::drafts::DraftSummary>,
    limit: usize,
) -> Result<ToolOutput, String> {
    drafts.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.project.cmp(&right.project))
    });
    let total = drafts.len();
    let mut rows = Vec::with_capacity(total.min(limit));
    for draft in drafts.into_iter().take(limit) {
        let draft_id =
            crate::drafts::draft_id_from_project(&draft.project).map_err(|internal| {
                error!(
                    project = %draft.project.display(),
                    error = %internal,
                    "draft listing returned a non-draft path"
                );
                "project_list_drafts failed: the draft store returned an invalid identity"
                    .to_string()
            })?;
        rows.push(json!({
            "draft_id": draft_id,
            "name": bounded_display_name(&draft.name),
            "modified_unix_ms": modified_unix_ms(draft.modified)
        }));
    }

    encode_output(
        PROJECT_LIST_DRAFTS,
        json!({
            "status": "ok",
            "drafts": rows,
            "total": total,
            "truncated": total > limit
        }),
    )
}

fn modified_unix_ms(modified: SystemTime) -> Option<u64> {
    let milliseconds = modified.duration_since(UNIX_EPOCH).ok()?.as_millis();
    u64::try_from(milliseconds).ok()
}

fn bounded_display_name(name: &str) -> String {
    name.chars().take(MAX_DISPLAY_NAME_CHARS).collect()
}

fn encode_output(tool: &'static str, value: Value) -> Result<ToolOutput, String> {
    let encoded = serde_json::to_string(&value).map_err(|internal| {
        error!(tool, error = %internal, "could not encode project-tool response");
        format!("{tool} failed: its response could not be encoded")
    })?;
    if encoded.len() > MAX_OUTPUT_BYTES {
        error!(
            tool,
            bytes = encoded.len(),
            "project-tool response exceeded its output bound"
        );
        return Err(format!(
            "{tool} failed: its response exceeded the output limit"
        ));
    }
    Ok(ToolOutput::text(encoded))
}

fn spec(name: &str, description: &str, parameters: Value, tier: ToolTier) -> HostToolSpec {
    HostToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
        tier,
    }
}

fn list_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_DRAFT_LIMIT,
                "default": DEFAULT_DRAFT_LIMIT,
                "description": "Maximum number of newest drafts to return."
            }
        },
        "required": []
    })
}

fn empty_object_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {},
        "required": []
    })
}

fn open_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "draft_id": {
                "type": "string",
                "minLength": MIN_DRAFT_ID_CHARS,
                "maxLength": MAX_DRAFT_ID_CHARS,
                "description": "Canonical app-owned draft identity returned by project_list_drafts."
            }
        },
        "required": ["draft_id"]
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    ListDrafts { limit: usize },
    Save,
    Open { draft_id: String },
}

fn parse_request(name: &str, arguments: &Value) -> Result<Request, String> {
    if !TOOL_NAMES.contains(&name) {
        return Err("unknown project tool".into());
    }
    let object = arguments
        .as_object()
        .ok_or_else(|| format!("{name} arguments must be an object"))?;

    match name {
        PROJECT_LIST_DRAFTS => parse_list_request(object),
        PROJECT_SAVE => {
            if !object.is_empty() {
                return Err("project_save does not accept arguments".into());
            }
            Ok(Request::Save)
        }
        PROJECT_OPEN => parse_open_request(object),
        _ => Err("unknown project tool".into()),
    }
}

fn parse_list_request(object: &Map<String, Value>) -> Result<Request, String> {
    if object.len() > 1 || object.keys().any(|key| key != "limit") {
        return Err("project_list_drafts has an unknown argument".into());
    }
    let Some(value) = object.get("limit") else {
        return Ok(Request::ListDrafts {
            limit: DEFAULT_DRAFT_LIMIT,
        });
    };
    let limit = value.as_u64().ok_or_else(|| {
        "project_list_drafts argument 'limit' must be an integer from 1 through 100".to_string()
    })?;
    if !(1..=MAX_DRAFT_LIMIT as u64).contains(&limit) {
        return Err(
            "project_list_drafts argument 'limit' must be an integer from 1 through 100".into(),
        );
    }
    Ok(Request::ListDrafts {
        limit: limit as usize,
    })
}

fn parse_open_request(object: &Map<String, Value>) -> Result<Request, String> {
    if object.len() != 1 || object.keys().any(|key| key != "draft_id") {
        return Err("project_open requires only the 'draft_id' argument".into());
    }
    let draft_id = object
        .get("draft_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "project_open argument 'draft_id' must be a string".to_string())?;
    let chars = draft_id.chars().count();
    if !(MIN_DRAFT_ID_CHARS..=MAX_DRAFT_ID_CHARS).contains(&chars) {
        return Err(format!(
            "project_open argument 'draft_id' must contain {MIN_DRAFT_ID_CHARS} through {MAX_DRAFT_ID_CHARS} characters"
        ));
    }
    Ok(Request::Open {
        draft_id: draft_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn draft(
        id: &str,
        name: impl Into<String>,
        modified: SystemTime,
    ) -> crate::drafts::DraftSummary {
        crate::drafts::DraftSummary {
            name: name.into(),
            project: crate::drafts::project_file(&crate::drafts::root_dir().join(id)),
            modified,
        }
    }

    fn output_json(output: &ToolOutput) -> Value {
        assert!(output.images.is_empty());
        serde_json::from_str(&output.text).expect("compact JSON project-tool output")
    }

    fn missing_draft_id() -> String {
        let mut time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time after epoch")
            .as_nanos();
        loop {
            // Production ids use epoch milliseconds, so an epoch-nanosecond
            // group is also well outside ids created during this test.
            let id = format!("{time:x}-ffffffffffffffff");
            let project = crate::drafts::project_file(&crate::drafts::root_dir().join(&id));
            if !project.exists() {
                return id;
            }
            time = time.checked_add(1).expect("draft-id search overflow");
        }
    }

    #[test]
    fn specs_have_exact_names_tiers_and_strict_schemas() {
        let registry = specs();
        assert_eq!(
            registry
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            TOOL_NAMES
        );
        assert_eq!(
            registry.iter().map(|entry| entry.tier).collect::<Vec<_>>(),
            [ToolTier::ReadOnly, ToolTier::Workspace, ToolTier::System]
        );
        assert_eq!(registry[0].parameters, list_schema());
        assert_eq!(registry[1].parameters, empty_object_schema());
        assert_eq!(registry[2].parameters, open_schema());
        assert_eq!(registry[0].parameters["properties"]["limit"]["minimum"], 1);
        assert_eq!(
            registry[0].parameters["properties"]["limit"]["maximum"],
            100
        );
        assert_eq!(registry[0].parameters["properties"]["limit"]["default"], 50);
        assert_eq!(
            registry[2].parameters["properties"]["draft_id"]["minLength"],
            MIN_DRAFT_ID_CHARS
        );
        assert_eq!(
            registry[2].parameters["properties"]["draft_id"]["maxLength"],
            MAX_DRAFT_ID_CHARS
        );
        assert_eq!(registry[2].parameters["required"], json!(["draft_id"]));
        for entry in registry {
            assert_eq!(entry.parameters["type"], "object");
            assert_eq!(entry.parameters["additionalProperties"], false);
            assert!(!entry.description.is_empty());
        }
    }

    #[test]
    fn parser_rejects_non_objects_unknown_fields_and_non_integer_limits() {
        for arguments in [Value::Null, json!([]), json!(""), json!(false), json!(1)] {
            assert!(parse_request(PROJECT_LIST_DRAFTS, &arguments).is_err());
            assert!(parse_request(PROJECT_SAVE, &arguments).is_err());
            assert!(parse_request(PROJECT_OPEN, &arguments).is_err());
        }
        for arguments in [
            json!({"extra": 1}),
            json!({"limit": 1, "extra": 2}),
            json!({"limit": null}),
            json!({"limit": false}),
            json!({"limit": 1.0}),
            json!({"limit": 0}),
            json!({"limit": -1}),
            json!({"limit": 101}),
        ] {
            assert!(
                parse_request(PROJECT_LIST_DRAFTS, &arguments).is_err(),
                "{arguments}"
            );
        }
        assert!(parse_request(PROJECT_SAVE, &json!({"limit": 1})).is_err());
        for arguments in [
            json!({}),
            json!({"extra": "abc-1"}),
            json!({"draft_id": "abc-1", "extra": true}),
            json!({"draft_id": null}),
            json!({"draft_id": false}),
            json!({"draft_id": 1}),
            json!({"draft_id": ""}),
            json!({"draft_id": "a"}),
            json!({"draft_id": "x".repeat(MAX_DRAFT_ID_CHARS + 1)}),
        ] {
            assert!(
                parse_request(PROJECT_OPEN, &arguments).is_err(),
                "{arguments}"
            );
        }
        assert!(parse_request("project_future", &json!({})).is_err());
        assert_eq!(
            parse_request(PROJECT_LIST_DRAFTS, &json!({})),
            Ok(Request::ListDrafts {
                limit: DEFAULT_DRAFT_LIMIT
            })
        );
        assert_eq!(
            parse_request(PROJECT_LIST_DRAFTS, &json!({"limit": 100})),
            Ok(Request::ListDrafts { limit: 100 })
        );
        assert_eq!(parse_request(PROJECT_SAVE, &json!({})), Ok(Request::Save));
        assert_eq!(
            parse_request(PROJECT_OPEN, &json!({"draft_id": "abc-1"})),
            Ok(Request::Open {
                draft_id: "abc-1".into()
            })
        );
    }

    #[test]
    fn open_validation_rejects_malformed_and_missing_ids_without_path_leaks() {
        let private_path = "/private/agent-secret/project.cutlass";
        let malformed = validate_request(
            PROJECT_OPEN,
            &json!({
                "draft_id": private_path
            }),
        )
        .expect_err("filesystem paths are not draft ids");
        assert_eq!(
            malformed,
            "project_open argument 'draft_id' must be a canonical app-owned draft ID"
        );
        assert!(!malformed.contains("/private"));
        assert!(!malformed.contains("agent-secret"));

        let missing_id = missing_draft_id();
        let missing = validate_request(PROJECT_OPEN, &json!({"draft_id": missing_id}))
            .expect_err("missing draft must fail read-only preflight");
        assert!(missing.starts_with("project_open failed:"));
        assert!(
            !missing.contains(crate::drafts::root_dir().to_string_lossy().as_ref()),
            "{missing}"
        );
        assert!(!missing.contains("project.cutlass"), "{missing}");
        assert!(missing.len() < 160);
    }

    #[test]
    fn draft_serialization_is_deterministic_bounded_sorted_and_path_free() {
        let before_epoch = UNIX_EPOCH
            .checked_sub(Duration::from_millis(1))
            .expect("pre-epoch time");
        let oversized_name = "é".repeat(MAX_DISPLAY_NAME_CHARS + 20);
        let fixtures = vec![
            draft("abc-1", "old", UNIX_EPOCH + Duration::from_millis(1_000)),
            draft(
                "abc-3",
                oversized_name,
                UNIX_EPOCH + Duration::from_millis(3_000),
            ),
            draft("abc-0", "pre-epoch", before_epoch),
            draft("abc-2", "middle", UNIX_EPOCH + Duration::from_millis(2_000)),
        ];

        let first = list_drafts_output(fixtures.clone(), 2).expect("serialize drafts");
        let second = list_drafts_output(fixtures, 2).expect("serialize drafts again");
        assert_eq!(first, second);
        assert!(!first.text.contains('\n'), "response must be compact JSON");
        assert!(
            !first
                .text
                .contains(crate::drafts::root_dir().to_string_lossy().as_ref())
        );
        assert!(!first.text.contains("project.cutlass"));

        let value = output_json(&first);
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["drafts", "status", "total", "truncated"])
        );
        assert_eq!(value["status"], "ok");
        assert_eq!(value["total"], 4);
        assert_eq!(value["truncated"], true);
        let rows = value["drafts"].as_array().expect("draft rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["draft_id"], "abc-3");
        assert_eq!(rows[0]["modified_unix_ms"], 3_000);
        assert_eq!(
            rows[0]["name"].as_str().unwrap().chars().count(),
            MAX_DISPLAY_NAME_CHARS
        );
        assert_eq!(rows[1]["draft_id"], "abc-2");
        assert_eq!(
            rows[0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["draft_id", "modified_unix_ms", "name"])
        );

        let all = output_json(
            &list_drafts_output(
                vec![draft("abc-0", "pre-epoch", before_epoch)],
                MAX_DRAFT_LIMIT,
            )
            .expect("serialize pre-epoch draft"),
        );
        assert!(all["drafts"][0]["modified_unix_ms"].is_null());
        assert_eq!(all["truncated"], false);
    }

    #[test]
    fn open_output_is_bounded_path_free_and_bound_to_the_requested_identity() {
        let draft_id = "abcdef-12";
        let project = crate::drafts::project_file(&crate::drafts::root_dir().join(draft_id));
        let opened = OpenProjectRpcResult {
            path: project.clone(),
            project_name: "é".repeat(MAX_DISPLAY_NAME_CHARS + 20),
            missing_media_count: 7,
        };
        let output = open_output(draft_id, &opened).expect("safe open output");
        assert!(!output.text.contains('\n'), "response must be compact JSON");
        assert!(!output.text.contains(project.to_string_lossy().as_ref()));
        assert!(!output.text.contains("project.cutlass"));

        let value = output_json(&output);
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["draft_id", "missing_media_count", "project_name", "status"])
        );
        assert_eq!(value["status"], "ok");
        assert_eq!(value["draft_id"], draft_id);
        assert_eq!(value["missing_media_count"], 7);
        assert_eq!(
            value["project_name"].as_str().unwrap().chars().count(),
            MAX_DISPLAY_NAME_CHARS
        );

        let mismatch =
            open_output("abcdef-13", &opened).expect_err("identity mismatch must fail closed");
        assert!(mismatch.contains("current session was replaced"));
        assert!(!mismatch.contains(project.to_string_lossy().as_ref()));
        assert!(!mismatch.contains("project.cutlass"));

        let outside = OpenProjectRpcResult {
            path: PathBuf::from("/private/agent-secret/project.cutlass"),
            project_name: "outside".into(),
            missing_media_count: 0,
        };
        let outside_error =
            open_output(draft_id, &outside).expect_err("outside path must fail closed");
        assert!(outside_error.contains("current session was replaced"));
        assert!(!outside_error.contains("/private"));
        assert!(!outside_error.contains("agent-secret"));
    }

    #[test]
    fn open_rpc_errors_preserve_not_started_and_unknown_outcomes_without_paths() {
        let cancelled = public_open_error(
            "open project request cancelled before worker claim; not started for \
             /private/agent-secret/project.cutlass",
        );
        assert_eq!(cancelled, "project_open was cancelled before it started");
        assert!(!cancelled.contains("/private"));

        let not_started = public_open_error(
            "open project request failed: preview worker is not running; not started for \
             /private/agent-secret/project.cutlass",
        );
        assert_eq!(
            not_started,
            "project_open failed before the editor started it"
        );
        assert!(!not_started.contains("/private"));

        for internal in [
            "open project request timed out after worker claim; outcome unknown after 30000 ms \
             for /private/agent-secret/project.cutlass",
            "open project outcome uncertain/partially committed: \
             /private/agent-secret/project.cutlass replaced the session",
        ] {
            let unknown = public_open_error(internal);
            assert!(unknown.contains("outcome unknown"), "{unknown}");
            assert!(unknown.contains("project may have opened"), "{unknown}");
            assert!(!unknown.contains("/private"), "{unknown}");
            assert!(!unknown.contains("agent-secret"), "{unknown}");
            assert!(unknown.len() < 160);
        }

        let failed = public_open_error(
            "open project failed for /private/agent-secret/project.cutlass: corrupt data",
        );
        assert_eq!(
            failed,
            "project_open failed: the requested draft could not be opened"
        );
        assert!(!failed.contains("/private"));
        assert!(!failed.contains("agent-secret"));
        assert!(!failed.contains("may have opened"));
    }

    #[test]
    fn save_output_contains_only_safe_identity_and_clean_state() {
        let project = crate::drafts::project_file(&crate::drafts::root_dir().join("abcdef-12"));
        let output = save_output(&project, false).expect("safe save output");
        assert!(!output.text.contains(project.to_string_lossy().as_ref()));
        assert!(!output.text.contains("project.cutlass"));
        let value = output_json(&output);
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["dirty", "draft_id", "status"])
        );
        assert_eq!(
            value,
            json!({"status":"ok","draft_id":"abcdef-12","dirty":false})
        );

        let outside = PathBuf::from("/private/agent-secret/project.cutlass");
        let error = save_output(&outside, false).expect_err("outside path must fail");
        assert!(!error.contains("/private"));
        assert!(!error.contains("agent-secret"));

        let dirty_error = save_output(&project, true).expect_err("dirty save must fail");
        assert!(!dirty_error.contains(project.to_string_lossy().as_ref()));

        let internal = "save failed for /private/agent-secret/project.cutlass: disk error";
        let public = public_save_error(internal);
        assert_eq!(
            public,
            "project_save failed: the current draft could not be saved"
        );
        assert!(!public.contains("/private"));
        assert!(!public.contains("agent-secret"));
    }

    #[test]
    fn unavailable_worker_is_an_honest_bounded_error() {
        let error = call(None, PROJECT_SAVE, &json!({}), &AtomicBool::new(false))
            .expect_err("missing worker");
        assert_eq!(
            error,
            "project_save failed: the editor worker is unavailable"
        );
        assert!(error.len() < 128);

        let cancelled = call(
            None,
            PROJECT_OPEN,
            &json!({"draft_id": "abc-1"}),
            &AtomicBool::new(true),
        )
        .expect_err("pre-cancelled open must stop before draft resolution");
        assert_eq!(cancelled, "project_open was cancelled before it started");
    }

    #[test]
    fn live_project_mutation_classification_is_explicit() {
        assert!(!mutates_live_project(PROJECT_LIST_DRAFTS));
        assert!(mutates_live_project(PROJECT_SAVE));
        assert!(mutates_live_project(PROJECT_OPEN));
        assert!(
            mutates_live_project("project_future"),
            "future project tools must fail closed until explicitly classified as read-only"
        );
        assert!(!mutates_live_project("app_state"));
    }
}
