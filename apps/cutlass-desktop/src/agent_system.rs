//! System-tier desktop tools for explicit external process handoff.
//!
//! The approval broker in `agent.rs` runs before this module. Validation and
//! cancellation checks remain local so direct callers cannot bypass the same
//! safety boundary accidentally.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use cutlass_ai::{HostToolSpec, ToolOutput, ToolTier};
use cutlass_storage::CacheId;
use serde_json::{Map, Value, json};

use crate::cache_registry::CacheRegistry;

const SYSTEM_REVEAL: &str = "system_reveal";
const SYSTEM_OPEN_EXTERNAL: &str = "system_open_external";
const SYSTEM_CACHE_LIST: &str = "system_cache_list";
const SYSTEM_CACHE_CLEAR: &str = "system_cache_clear";
const TOOL_NAMES: [&str; 4] = [
    SYSTEM_REVEAL,
    SYSTEM_OPEN_EXTERNAL,
    SYSTEM_CACHE_LIST,
    SYSTEM_CACHE_CLEAR,
];
const CACHE_IDS: [&str; 10] = [
    "preview_frames",
    "library_thumbnails",
    "timeline_filmstrips",
    "timeline_waveforms",
    "proxies",
    "download",
    "catalog",
    "luts",
    "lottie",
    "templates",
];

pub fn specs() -> Vec<HostToolSpec> {
    vec![
        spec(
            SYSTEM_REVEAL,
            "Reveal an existing absolute file or directory in Finder, Explorer, or the platform file browser.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Existing absolute path to reveal."
                    }
                },
                "required": ["path"]
            }),
        ),
        spec(
            SYSTEM_OPEN_EXTERNAL,
            "Open an HTTP(S) URL or existing absolute path in the user's external default application.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "target": {
                        "type": "string",
                        "minLength": 1,
                        "description": "HTTP(S) URL or existing absolute path."
                    }
                },
                "required": ["target"]
            }),
        ),
        spec(
            SYSTEM_CACHE_LIST,
            "List exact memory and disk usage for every registered application cache.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {},
                "required": []
            }),
        ),
        spec(
            SYSTEM_CACHE_CLEAR,
            "Clear exactly one registered cache and report the removed and remaining usage.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "cache_id": {
                        "type": "string",
                        "enum": CACHE_IDS,
                        "description": "Stable identifier of the one cache to clear."
                    }
                },
                "required": ["cache_id"]
            }),
        ),
    ]
}

pub fn call(
    cache_registry: Option<&CacheRegistry>,
    name: &str,
    arguments: &Value,
    cancel: &AtomicBool,
) -> Result<ToolOutput, String> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled before the system tool could run".into());
    }
    let request = parse_request(name, arguments)?;
    if cancel.load(Ordering::Acquire) {
        return Err(
            if matches!(&request, Request::Reveal(_) | Request::Open(_)) {
                "cancelled before the external application could open".into()
            } else {
                "cancelled before the cache operation could run".into()
            },
        );
    }

    let result = match request {
        Request::Reveal(path) => {
            crate::external::reveal_path(&path)?;
            json!({
                "status": "requested",
                "action": "reveal",
                "path": path.to_string_lossy()
            })
        }
        Request::Open(target) => {
            crate::external::open_external(&target)?;
            json!({
                "status": "requested",
                "action": "open-external",
                "target": target_label(&target)
            })
        }
        Request::CacheList => {
            let registry =
                cache_registry.ok_or_else(|| "cache registry is unavailable".to_string())?;
            let caches = registry.snapshot_all(cancel)?;
            json!({ "caches": caches })
        }
        Request::CacheClear(id) => {
            let registry =
                cache_registry.ok_or_else(|| "cache registry is unavailable".to_string())?;
            let report = registry.clear(id, cancel)?;
            json!(report)
        }
    };
    serde_json::to_string(&result)
        .map(ToolOutput::text)
        .map_err(|error| format!("could not encode system-tool response: {error}"))
}

fn spec(name: &str, description: &str, parameters: Value) -> HostToolSpec {
    HostToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
        tier: ToolTier::System,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    Reveal(PathBuf),
    Open(crate::external::ExternalTarget),
    CacheList,
    CacheClear(CacheId),
}

fn parse_request(name: &str, arguments: &Value) -> Result<Request, String> {
    if !TOOL_NAMES.contains(&name) {
        return Err(format!("unknown system tool '{name}'"));
    }
    let object = arguments
        .as_object()
        .ok_or_else(|| format!("{name} arguments must be an object"))?;

    match name {
        SYSTEM_REVEAL => {
            validate_single_string(name, object, "path")?;
            let path = PathBuf::from(string_argument(name, object, "path")?);
            crate::external::validate_existing_absolute_path(&path)?;
            Ok(Request::Reveal(path))
        }
        SYSTEM_OPEN_EXTERNAL => {
            validate_single_string(name, object, "target")?;
            crate::external::parse_target(string_argument(name, object, "target")?)
                .map(Request::Open)
        }
        SYSTEM_CACHE_LIST => {
            validate_no_fields(name, object)?;
            Ok(Request::CacheList)
        }
        SYSTEM_CACHE_CLEAR => {
            if object.keys().any(|key| key != "cache_id") {
                return Err("system_cache_clear has an unknown argument".into());
            }
            validate_single_string(name, object, "cache_id")?;
            CacheId::parse(string_argument(name, object, "cache_id")?)
                .map(Request::CacheClear)
                .map_err(|_| "system_cache_clear argument 'cache_id' is unknown".to_string())
        }
        _ => Err(format!("unknown system tool '{name}'")),
    }
}

fn validate_no_fields(tool: &str, object: &Map<String, Value>) -> Result<(), String> {
    if !object.is_empty() {
        return Err(format!("{tool} does not accept arguments"));
    }
    Ok(())
}

fn validate_single_string(
    tool: &str,
    object: &Map<String, Value>,
    field: &str,
) -> Result<(), String> {
    if let Some(extra) = object.keys().find(|key| key.as_str() != field) {
        return Err(format!("{tool} has unknown argument '{extra}'"));
    }
    let value = string_argument(tool, object, field)?;
    if value.trim().is_empty() {
        return Err(format!("{tool} argument '{field}' must not be empty"));
    }
    Ok(())
}

fn string_argument<'a>(
    tool: &str,
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .ok_or_else(|| format!("{tool} is missing required argument '{field}'"))?
        .as_str()
        .ok_or_else(|| format!("{tool} argument '{field}' must be a string"))
}

fn target_label(target: &crate::external::ExternalTarget) -> String {
    match target {
        crate::external::ExternalTarget::WebUrl(url) => url.clone(),
        crate::external::ExternalTarget::Path(path) => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use super::*;

    #[test]
    fn registry_is_exact_unique_and_entirely_system_tier() {
        let registry = specs();
        assert_eq!(
            registry
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            TOOL_NAMES
        );
        assert_eq!(
            registry
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            TOOL_NAMES.len()
        );
        for entry in &registry {
            assert_eq!(entry.tier, ToolTier::System);
            assert_eq!(entry.parameters["type"], "object");
            assert_eq!(entry.parameters["additionalProperties"], false);
            assert!(!entry.description.is_empty());
        }
        assert_eq!(
            registry
                .iter()
                .find(|entry| entry.name == SYSTEM_CACHE_CLEAR)
                .unwrap()
                .parameters["properties"]["cache_id"]["enum"],
            json!(CACHE_IDS)
        );
    }

    #[test]
    fn parser_accepts_valid_reveal_and_open_targets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("clip.mp4");
        fs::write(&file, b"fixture").expect("write");
        assert_eq!(
            parse_request(SYSTEM_REVEAL, &json!({ "path": file.to_string_lossy() })),
            Ok(Request::Reveal(file.clone()))
        );
        assert_eq!(
            parse_request(
                SYSTEM_OPEN_EXTERNAL,
                &json!({ "target": file.to_string_lossy() })
            ),
            Ok(Request::Open(crate::external::ExternalTarget::Path(file)))
        );
        assert_eq!(
            parse_request(
                SYSTEM_OPEN_EXTERNAL,
                &json!({ "target": "https://cutlass.sh/docs" })
            ),
            Ok(Request::Open(crate::external::ExternalTarget::WebUrl(
                "https://cutlass.sh/docs".into()
            )))
        );
    }

    #[test]
    fn parser_accepts_strict_cache_requests() {
        assert_eq!(
            parse_request(SYSTEM_CACHE_LIST, &json!({})),
            Ok(Request::CacheList)
        );
        for id in CacheId::ALL {
            assert_eq!(
                parse_request(SYSTEM_CACHE_CLEAR, &json!({ "cache_id": id.as_str() })),
                Ok(Request::CacheClear(id))
            );
        }
    }

    #[test]
    fn parser_rejects_unknown_names_shapes_fields_and_unsafe_targets() {
        assert!(parse_request("system_missing", &json!({})).is_err());
        assert!(parse_request(SYSTEM_REVEAL, &Value::Null).is_err());
        assert!(parse_request(SYSTEM_REVEAL, &json!({})).is_err());
        assert!(parse_request(SYSTEM_REVEAL, &json!({ "path": 1 })).is_err());
        assert!(
            parse_request(
                SYSTEM_REVEAL,
                &json!({ "path": "/tmp", "unexpected": true })
            )
            .is_err()
        );
        assert!(parse_request(SYSTEM_REVEAL, &json!({ "path": "relative/file" })).is_err());
        assert!(
            parse_request(
                SYSTEM_OPEN_EXTERNAL,
                &json!({ "target": "javascript:alert(1)" })
            )
            .is_err()
        );
        assert!(
            parse_request(
                SYSTEM_OPEN_EXTERNAL,
                &json!({ "target": "file:///etc/passwd" })
            )
            .is_err()
        );
        assert!(parse_request(SYSTEM_CACHE_LIST, &json!({ "all": true })).is_err());
        assert!(parse_request(SYSTEM_CACHE_CLEAR, &json!({})).is_err());
        assert!(parse_request(SYSTEM_CACHE_CLEAR, &json!({ "cache_id": 1 })).is_err());
        assert!(parse_request(SYSTEM_CACHE_CLEAR, &json!({ "cache_id": "all" })).is_err());
        assert!(
            parse_request(
                SYSTEM_CACHE_CLEAR,
                &json!({ "cache_id": "download", "unexpected": true })
            )
            .is_err()
        );

        let oversized_id = parse_request(
            SYSTEM_CACHE_CLEAR,
            &json!({ "cache_id": "x".repeat(10_000) }),
        )
        .unwrap_err();
        assert!(oversized_id.len() < 128);
        let mut oversized_field = Map::new();
        oversized_field.insert("x".repeat(10_000), Value::Bool(true));
        let oversized_field =
            parse_request(SYSTEM_CACHE_CLEAR, &Value::Object(oversized_field)).unwrap_err();
        assert!(oversized_field.len() < 128);
    }

    #[test]
    fn call_honors_cancellation_before_parsing_or_process_launch() {
        let cancel = AtomicBool::new(true);
        let error = call(None, SYSTEM_OPEN_EXTERNAL, &Value::Null, &cancel).unwrap_err();
        assert!(error.contains("cancelled"));
    }
}
