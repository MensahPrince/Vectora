//! Strict, path-free project tools for the desktop agent.
//!
//! Draft paths remain an internal engine/storage detail. The model sees only
//! canonical draft identities and bounded display metadata.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use cutlass_ai::{HostToolSpec, ToolOutput, ToolTier};
use serde_json::{Map, Value, json};
use tracing::error;

use crate::preview_worker::WorkerHandle;

const PROJECT_LIST_DRAFTS: &str = "project_list_drafts";
const PROJECT_SAVE: &str = "project_save";
const TOOL_NAMES: [&str; 2] = [PROJECT_LIST_DRAFTS, PROJECT_SAVE];

const DEFAULT_DRAFT_LIMIT: usize = 50;
const MAX_DRAFT_LIMIT: usize = 100;
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
    ]
}

/// Whether a registered project tool can change live project/session state.
///
/// The read-only allowlist is explicit; unknown future project tools fail
/// closed as mutations until they are deliberately classified.
pub(crate) fn mutates_live_project(name: &str) -> bool {
    cutlass_ai::namespace(name) == "project" && name != PROJECT_LIST_DRAFTS
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
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    ListDrafts { limit: usize },
    Save,
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
            [ToolTier::ReadOnly, ToolTier::Workspace]
        );
        assert_eq!(registry[0].parameters, list_schema());
        assert_eq!(registry[1].parameters, empty_object_schema());
        assert_eq!(registry[0].parameters["properties"]["limit"]["minimum"], 1);
        assert_eq!(
            registry[0].parameters["properties"]["limit"]["maximum"],
            100
        );
        assert_eq!(registry[0].parameters["properties"]["limit"]["default"], 50);
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
    }

    #[test]
    fn live_project_mutation_classification_is_explicit() {
        assert!(!mutates_live_project(PROJECT_LIST_DRAFTS));
        assert!(mutates_live_project(PROJECT_SAVE));
        assert!(
            mutates_live_project("project_future"),
            "future project tools must fail closed until explicitly classified as read-only"
        );
        assert!(!mutates_live_project("app_state"));
    }
}
