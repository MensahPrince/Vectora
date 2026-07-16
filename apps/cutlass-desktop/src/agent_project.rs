//! Strict project tools for the desktop agent.
//!
//! Draft paths remain an internal engine/storage detail. The one external-path
//! tool canonicalizes an existing regular media file, never returns that path
//! to the model, and dispatches only through the acknowledged preview-worker
//! RPC.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use cutlass_ai::{HostToolSpec, ToolOutput, ToolTier};
use serde_json::{Map, Value, json};
use tracing::error;

use crate::preview_worker::{ImportMediaRpcResult, OpenProjectRpcResult, WorkerHandle};

const PROJECT_LIST_DRAFTS: &str = "project_list_drafts";
const PROJECT_SAVE: &str = "project_save";
const PROJECT_OPEN: &str = "project_open";
pub(crate) const PROJECT_IMPORT_MEDIA: &str = "project_import_media";
const TOOL_NAMES: [&str; 4] = [
    PROJECT_LIST_DRAFTS,
    PROJECT_SAVE,
    PROJECT_OPEN,
    PROJECT_IMPORT_MEDIA,
];

const DEFAULT_DRAFT_LIMIT: usize = 50;
const MAX_DRAFT_LIMIT: usize = 100;
const MIN_DRAFT_ID_CHARS: usize = 3;
/// A canonical id has at most 32 time hex digits, one dash, and 16 sequence
/// hex digits. Canonical ids are ASCII, so this is also their byte bound.
pub(crate) const MAX_DRAFT_ID_CHARS: usize = 32 + 1 + 16;
const MAX_DISPLAY_NAME_CHARS: usize = 128;
/// Deliberately below platform path maxima so approval copy and diagnostics
/// stay bounded even after canonicalization.
pub(crate) const MAX_IMPORT_PATH_CHARS: usize = 1_024;
const MAX_OUTPUT_BYTES: usize = 128 * 1024;

/// Opaque proof that one import path passed the full regular-file preflight.
///
/// The private identity binds Ask-mode approval to the exact file inspected,
/// while callers may expose only the canonical path in approval copy.
pub(crate) struct ValidatedImportMedia {
    canonical_path: PathBuf,
    identity: ImportFileIdentity,
}

impl ValidatedImportMedia {
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    fn identifies_same_file(&self, other: &Self) -> bool {
        self.canonical_path == other.canonical_path && self.identity == other.identity
    }
}

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
        spec(
            PROJECT_IMPORT_MEDIA,
            "Import one existing external media file by absolute path. Cutlass references the source file rather than copying it.",
            import_media_schema(),
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
/// published. Path-bearing requests perform their read-only filesystem
/// preflight here; dispatch repeats it after approval.
pub(crate) fn validate_request(name: &str, arguments: &Value) -> Result<(), String> {
    match parse_request(name, arguments)? {
        Request::Open { draft_id } => resolve_open_draft(&draft_id).map(|_| ()),
        Request::ImportMedia { path } => preflight_import_media_path(&path).map(|_| ()),
        Request::ListDrafts { .. } | Request::Save => Ok(()),
    }
}

/// Return an opaque token for the exact regular file approved for import.
///
/// This intentionally performs the same full preflight as
/// [`validate_request`]. The host retains this value across an Ask-mode
/// approval and requires the post-approval preflight to match its path and
/// platform file identity.
pub(crate) fn validated_import_media(arguments: &Value) -> Result<ValidatedImportMedia, String> {
    match parse_request(PROJECT_IMPORT_MEDIA, arguments)? {
        Request::ImportMedia { path } => preflight_import_media_path(&path),
        _ => Err("project_import_media request could not be validated".into()),
    }
}

pub(crate) fn call(
    worker: Option<&WorkerHandle>,
    name: &str,
    arguments: &Value,
    cancel: &AtomicBool,
) -> Result<ToolOutput, String> {
    call_with_approved_import(worker, name, arguments, None, cancel)
}

pub(crate) fn call_with_approved_import(
    worker: Option<&WorkerHandle>,
    name: &str,
    arguments: &Value,
    approved_import: Option<&ValidatedImportMedia>,
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
        Request::ImportMedia { path } => import_media(worker, &path, approved_import, cancel),
    }
}

fn import_media(
    worker: Option<&WorkerHandle>,
    requested_path: &str,
    approved_import: Option<&ValidatedImportMedia>,
    cancel: &AtomicBool,
) -> Result<ToolOutput, String> {
    // Parsing is memory-only. Cancellation must win before the first
    // filesystem operation.
    if cancel.load(Ordering::Acquire) {
        return Err("project_import_media was cancelled before it started".into());
    }

    // Existing files are canonicalized and that canonical absolute regular
    // file is what the worker receives. Intermediate links may therefore be
    // resolved, but the requested final component and resolved endpoint must
    // not be a symlink/reparse point. Repeating this after approval narrows,
    // but cannot eliminate, the residual path TOCTOU before the path-based RPC
    // opens the source.
    let current = preflight_import_media_path(requested_path)?;
    if let Some(approved) = approved_import {
        ensure_approved_import_unchanged(approved, &current)?;
    }
    if cancel.load(Ordering::Acquire) {
        return Err("project_import_media was cancelled before it started".into());
    }

    let worker = worker.ok_or_else(|| {
        "project_import_media failed: the editor worker is unavailable; not started".to_string()
    })?;
    let canonical = current.canonical_path().to_path_buf();
    let imported = worker
        .import_media_rpc_with_cancel(canonical, cancel)
        .map_err(|internal| {
            error!(error = %internal, "agent media import RPC failed");
            public_import_error(&internal)
        })?;
    import_media_output(&current, &imported)
}

fn ensure_approved_import_unchanged(
    approved: &ValidatedImportMedia,
    current: &ValidatedImportMedia,
) -> Result<(), String> {
    if current.identifies_same_file(approved) {
        return Ok(());
    }
    error!(
        approved = %approved.canonical_path().display(),
        resolved = %current.canonical_path().display(),
        "agent media import path or file identity changed after approval"
    );
    Err("project_import_media failed: the media file changed after approval; not started".into())
}

fn public_import_error(internal: &str) -> String {
    if internal.contains("outcome unknown")
        || internal.contains("outcome uncertain/partially committed")
        || internal.contains("import succeeded")
    {
        "project_import_media was dispatched, but the editor did not confirm its result; outcome unknown: media may have been imported".into()
    } else if internal.contains("cancelled before worker claim; not started") {
        "project_import_media was cancelled before it started".into()
    } else if internal.contains("preview worker is not running; not started")
        || internal.contains("preview worker stopped before replying; not started")
        || internal.contains("timed out before worker claim; not started")
    {
        "project_import_media failed before the editor started it".into()
    } else {
        "project_import_media failed: the media file could not be imported".into()
    }
}

fn import_media_output(
    expected: &ValidatedImportMedia,
    imported: &ImportMediaRpcResult,
) -> Result<ToolOutput, String> {
    let actual = inspect_import_file(&imported.path).map_err(|internal| {
        error!(
            acknowledged = %imported.path.display(),
            error = %internal,
            "agent media import could not verify the acknowledged source path"
        );
        "project_import_media could not verify the acknowledged source file; media may have been imported"
            .to_string()
    })?;
    if !actual.identifies_same_file(expected) {
        error!(
            approved = %expected.canonical_path().display(),
            acknowledged = %actual.canonical_path().display(),
            media_id = imported.media_id,
            "agent media import acknowledged a different source path or file identity"
        );
        return Err(
            "project_import_media acknowledged a different source file; media may have been imported"
                .into(),
        );
    }
    let actual_path = actual.canonical_path();
    if imported.media_id == 0 {
        error!("agent media import acknowledged an invalid zero media id");
        return Err(
            "project_import_media returned an invalid media identity; media may have been imported"
                .into(),
        );
    }
    let filename = actual_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(bounded_safe_display_name)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            error!(
                acknowledged = %actual_path.display(),
                "agent media import source has no safe display filename"
            );
            "project_import_media could not produce a safe filename; media may have been imported"
                .to_string()
        })?;

    encode_output(
        PROJECT_IMPORT_MEDIA,
        json!({
            "status": "ok",
            "media_id": imported.media_id,
            "filename": filename
        }),
    )
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

fn bounded_safe_display_name(name: &str) -> String {
    sanitize_display_text(name, MAX_DISPLAY_NAME_CHARS)
}

pub(crate) fn import_path_approval_display(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let chars = text.chars().count();
    if !path.is_absolute()
        || chars == 0
        || chars > MAX_IMPORT_PATH_CHARS
        || text.contains('\0')
        || contains_lexical_dot_component(text)
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return None;
    }
    Some(sanitize_display_text(text, MAX_IMPORT_PATH_CHARS))
}

fn sanitize_display_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .take(max_chars)
        .map(|character| {
            if character.is_control() || is_bidi_formatting(character) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn is_bidi_formatting(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn preflight_import_media_path(path: &str) -> Result<ValidatedImportMedia, String> {
    inspect_import_file(Path::new(path))
}

fn inspect_import_file(path: &Path) -> Result<ValidatedImportMedia, String> {
    let initial_metadata = fs::symlink_metadata(path).map_err(|internal| {
        error!(
            path = %path.display(),
            error = %internal,
            "agent media import could not inspect requested path"
        );
        public_import_path_io_error(internal.kind())
    })?;
    ensure_import_regular_file(path, &initial_metadata)?;
    let initial_identity = import_file_identity(&initial_metadata);

    let canonical = fs::canonicalize(path).map_err(|internal| {
        error!(
            path = %path.display(),
            error = %internal,
            "agent media import could not canonicalize requested path"
        );
        public_import_path_io_error(internal.kind())
    })?;
    let Some(canonical_text) = canonical.to_str() else {
        error!(
            path = %canonical.display(),
            "agent media import canonical path is not valid Unicode"
        );
        return Err(
            "project_import_media failed: the resolved media path cannot be safely represented"
                .into(),
        );
    };
    if canonical_text.chars().count() > MAX_IMPORT_PATH_CHARS {
        error!(
            path = %canonical.display(),
            "agent media import canonical path exceeds its character bound"
        );
        return Err(
            "project_import_media failed: the resolved media path exceeds the safe length limit"
                .into(),
        );
    }

    let canonical_metadata = fs::symlink_metadata(&canonical).map_err(|internal| {
        error!(
            path = %canonical.display(),
            error = %internal,
            "agent media import could not inspect canonical path"
        );
        public_import_path_io_error(internal.kind())
    })?;
    ensure_import_regular_file(&canonical, &canonical_metadata)?;
    if initial_identity != import_file_identity(&canonical_metadata) {
        error!(
            requested = %path.display(),
            canonical = %canonical.display(),
            "agent media import file identity changed during canonicalization"
        );
        return Err(
            "project_import_media failed: the requested media file changed during safety checks; not started"
                .into(),
        );
    }

    // Reinspect both the final component and its resolution. This catches
    // ordinary replacement races inside the preflight window; the worker API
    // remains path-based, so a residual race still exists after return.
    let final_metadata = fs::symlink_metadata(path).map_err(|internal| {
        error!(
            path = %path.display(),
            error = %internal,
            "agent media import could not reinspect requested path"
        );
        public_import_path_io_error(internal.kind())
    })?;
    ensure_import_regular_file(path, &final_metadata)?;
    if initial_identity != import_file_identity(&final_metadata) {
        error!(
            path = %path.display(),
            "agent media import file identity changed during preflight"
        );
        return Err(
            "project_import_media failed: the requested media file changed during safety checks; not started"
                .into(),
        );
    }
    let verified_canonical = fs::canonicalize(path).map_err(|internal| {
        error!(
            path = %path.display(),
            error = %internal,
            "agent media import could not recanonicalize requested path"
        );
        public_import_path_io_error(internal.kind())
    })?;
    if verified_canonical != canonical {
        error!(
            first = %canonical.display(),
            second = %verified_canonical.display(),
            "agent media import canonical path changed during preflight"
        );
        return Err(
            "project_import_media failed: the requested media file changed during safety checks; not started"
                .into(),
        );
    }

    Ok(ValidatedImportMedia {
        canonical_path: canonical,
        identity: initial_identity,
    })
}

fn ensure_import_regular_file(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(metadata) {
        error!(
            path = %path.display(),
            "agent media import rejected a final symlink or reparse point"
        );
        return Err(
            "project_import_media argument 'path' must not name a symbolic link or reparse point"
                .into(),
        );
    }
    if !metadata.file_type().is_file() {
        error!(
            path = %path.display(),
            "agent media import rejected a non-regular file"
        );
        return Err("project_import_media argument 'path' must name a regular file".into());
    }
    Ok(())
}

fn public_import_path_io_error(kind: io::ErrorKind) -> String {
    if kind == io::ErrorKind::NotFound {
        "project_import_media failed: the requested media file does not exist".into()
    } else {
        "project_import_media failed: the requested media file could not be safely inspected".into()
    }
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct ImportFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn import_file_identity(metadata: &fs::Metadata) -> ImportFileIdentity {
    use std::os::unix::fs::MetadataExt as _;

    ImportFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

// `volume_serial_number()`/`file_index()` are still unstable
// (`windows_by_handle`), so approximate identity with stable metadata.
#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
struct ImportFileIdentity {
    creation_time: u64,
    last_write_time: u64,
    file_size: u64,
    file_attributes: u32,
}

#[cfg(windows)]
fn import_file_identity(metadata: &fs::Metadata) -> ImportFileIdentity {
    use std::os::windows::fs::MetadataExt as _;

    ImportFileIdentity {
        creation_time: metadata.creation_time(),
        last_write_time: metadata.last_write_time(),
        file_size: metadata.file_size(),
        file_attributes: metadata.file_attributes(),
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, PartialEq, Eq)]
struct ImportFileIdentity {
    len: u64,
    modified: Option<SystemTime>,
}

#[cfg(not(any(unix, windows)))]
fn import_file_identity(metadata: &fs::Metadata) -> ImportFileIdentity {
    ImportFileIdentity {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

#[cfg(windows)]
const WINDOWS_FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    metadata.file_attributes() & WINDOWS_FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
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

fn import_media_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_IMPORT_PATH_CHARS,
                "description": "Absolute path to one existing external regular media file."
            }
        },
        "required": ["path"]
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    ListDrafts { limit: usize },
    Save,
    Open { draft_id: String },
    ImportMedia { path: String },
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
        PROJECT_IMPORT_MEDIA => parse_import_media_request(object),
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

fn parse_import_media_request(object: &Map<String, Value>) -> Result<Request, String> {
    if object.len() != 1 || object.keys().any(|key| key != "path") {
        return Err("project_import_media requires only the 'path' argument".into());
    }
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "project_import_media argument 'path' must be a string".to_string())?;
    let chars = path.chars().take(MAX_IMPORT_PATH_CHARS + 1).count();
    if !(1..=MAX_IMPORT_PATH_CHARS).contains(&chars) {
        return Err(format!(
            "project_import_media argument 'path' must contain 1 through {MAX_IMPORT_PATH_CHARS} characters"
        ));
    }
    if path.contains('\0') {
        return Err("project_import_media argument 'path' must not contain NUL characters".into());
    }
    let parsed = Path::new(path);
    if !parsed.is_absolute() {
        return Err("project_import_media argument 'path' must be absolute".into());
    }
    if contains_lexical_dot_component(path)
        || parsed
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(
            "project_import_media argument 'path' must not contain '.' or '..' components".into(),
        );
    }
    Ok(Request::ImportMedia {
        path: path.to_owned(),
    })
}

fn contains_lexical_dot_component(path: &str) -> bool {
    path.split(|character| character == '/' || (cfg!(windows) && character == '\\'))
        .any(|component| matches!(component, "." | ".."))
}

#[cfg(test)]
mod tests;
