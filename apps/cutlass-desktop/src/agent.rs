//! AI agent worker: runs prompts against a sandbox engine, then replays
//! the validated plan on the live engine as one undoable history group.
//!
//! Why a sandbox? The agent loop holds a conversation across network
//! waits, and the engine's history groups don't nest — an open group on
//! the live engine would swallow any user edit made while the model
//! thinks. Instead the prompt edits a throwaway [`Engine`] seeded with a
//! snapshot of the live project: tool calls really apply (so the model
//! sees created clip/track ids and the world it changed), and nothing
//! touches the user's timeline until the plan replays atomically via
//! [`WorkerHandle::agent_apply_plan`]. Replay re-validates every step
//! against the live project and remaps ids the sandbox allocated, so a
//! mid-prompt user edit can only fail the plan loudly — never corrupt it.
//!
//! With the dry-run toggle on (the default), the plan is parked here and
//! the chat panel shows an Apply / Discard card instead of auto-applying.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use cutlass_ai::providers::{OpenAiProtocol, OpenAiProvider, ReasoningSummary};
use cutlass_ai::{
    AgentConfig, AgentEvent, AgentExtensions, EditorContext, EngineBridge, HostToolSpec, Message,
    ProjectSummary, PromptStatus, ToolHost, ToolOutput, ToolTier, WireCommand, compose_rules,
    expand_slash_command, load_agent_dir, merge_skills, run_prompt_with_host, summarize, validate,
};
use cutlass_commands::EditOutcome;
use cutlass_engine::{ApplyOutcome, Engine, EngineConfig};
use cutlass_jobs::JobManager;
use cutlass_settings::Autonomy;
use slint::{Model, ModelRc, SharedString, VecModel};
use tracing::{error, info, warn};

use crate::agent_senses::AgentSenses;
use crate::agent_session::{AgentSession, ChatMeta, TranscriptEntry};
use crate::cache_registry::CacheRegistry;
use crate::preview_worker::WorkerHandle;
use crate::{AgentEntry, AgentStore, AppWindow};

/// An entity id the sandbox allocated while rehearsing a command. Replay
/// maps it onto the id the live engine allocates for the same step.
#[derive(Debug, Clone, Copy)]
pub enum AgentCreated {
    Clip(u64),
    Track(u64),
    Marker(u64),
}

/// One rehearsed command, ready for live replay.
#[derive(Debug, Clone)]
pub struct AgentPlanStep {
    pub command: WireCommand,
    /// Sandbox id this step created (`split_clip`'s right half,
    /// `add_track`'s lane, …), if any.
    pub created: Option<AgentCreated>,
}

enum AgentRequest {
    Prompt {
        prompt: String,
        context: EditorContext,
        dry_run: bool,
    },
    ApplyPlan,
    DiscardPlan,
    NewChat,
    SelectChat {
        id: String,
    },
    /// Persist the outgoing draft's conversation and restore the incoming
    /// draft. A missing path means no app-owned project is active.
    SwitchProject {
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalChoice {
    Approve,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApprovalDecision {
    request_id: u64,
    choice: ApprovalChoice,
}

#[derive(Clone)]
pub struct AgentHandle {
    tx: Sender<AgentRequest>,
    cancel: Arc<AtomicBool>,
    approval_tx: Sender<ApprovalDecision>,
    pending_approval_id: Arc<AtomicU64>,
}

impl AgentHandle {
    pub fn prompt(&self, prompt: String, context: EditorContext, dry_run: bool) {
        let _ = self.tx.send(AgentRequest::Prompt {
            prompt,
            context,
            dry_run,
        });
    }

    pub fn apply_plan(&self) {
        let _ = self.tx.send(AgentRequest::ApplyPlan);
    }

    pub fn discard_plan(&self) {
        let _ = self.tx.send(AgentRequest::DiscardPlan);
    }

    pub fn new_chat(&self) {
        let _ = self.tx.send(AgentRequest::NewChat);
    }

    pub fn select_chat(&self, id: String) {
        let _ = self.tx.send(AgentRequest::SelectChat { id });
    }

    /// Persist the outgoing session and restore the incoming draft's
    /// conversation. Fired after the worker publishes a new project path.
    pub fn switch_project(&self, path: Option<PathBuf>) {
        let _ = self.tx.send(AgentRequest::SwitchProject { path });
    }

    /// Cooperative cancel: the provider checks this flag between stream
    /// chunks, so a running prompt aborts within one network read.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Approve only the System-tier call that is pending right now. The id
    /// travels with the decision so a delayed duplicate click can never
    /// authorize a later call.
    pub fn approve_system_tool(&self) {
        self.decide_system_tool(ApprovalChoice::Approve);
    }

    /// Decline only the System-tier call that is pending right now.
    pub fn deny_system_tool(&self) {
        self.decide_system_tool(ApprovalChoice::Deny);
    }

    fn decide_system_tool(&self, choice: ApprovalChoice) {
        let request_id = self.pending_approval_id.load(Ordering::Acquire);
        if request_id != 0 {
            let _ = self
                .approval_tx
                .send(ApprovalDecision { request_id, choice });
        }
    }
}

pub struct AgentWorker {
    handle: AgentHandle,
    _join: JoinHandle<()>,
}

struct AgentRuntimeHandles {
    store: slint::Weak<AgentStore<'static>>,
    app: slint::Weak<AppWindow>,
    cache_registry: CacheRegistry,
    job_manager: JobManager,
}

impl AgentWorker {
    pub fn spawn(
        worker: WorkerHandle,
        store: slint::Weak<AgentStore<'static>>,
        app: slint::Weak<AppWindow>,
        cache_registry: CacheRegistry,
        job_manager: JobManager,
    ) -> Result<Self, String> {
        let (tx, rx) = unbounded();
        let (approval_tx, approval_rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let pending_approval_id = Arc::new(AtomicU64::new(0));
        let thread_pending_approval_id = pending_approval_id.clone();
        let approval_id_allocator = Arc::new(AtomicU64::new(0));
        let join = std::thread::Builder::new()
            .name("cutlass-agent".into())
            .spawn(move || {
                agent_main(
                    worker,
                    AgentRuntimeHandles {
                        store,
                        app,
                        cache_registry,
                        job_manager,
                    },
                    rx,
                    thread_cancel,
                    approval_rx,
                    thread_pending_approval_id,
                    approval_id_allocator,
                )
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            handle: AgentHandle {
                tx,
                cancel,
                approval_tx,
                pending_approval_id,
            },
            _join: join,
        })
    }

    pub fn handle(&self) -> AgentHandle {
        self.handle.clone()
    }
}

fn entry(kind: &str, text: impl Into<SharedString>) -> AgentEntry {
    AgentEntry {
        kind: kind.into(),
        text: text.into(),
        image: Default::default(),
        image_aspect: 0.0,
    }
}

fn with_transcript(
    store: &slint::Weak<AgentStore<'static>>,
    f: impl FnOnce(&VecModel<AgentEntry>) + Send + 'static,
) {
    let store = store.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(store) = store.upgrade() {
            let transcript = store.get_transcript();
            if let Some(model) = transcript.as_any().downcast_ref::<VecModel<AgentEntry>>() {
                f(model);
            }
        }
    });
}

fn push_entry(store: &slint::Weak<AgentStore<'static>>, kind: &'static str, text: String) {
    with_transcript(store, move |model| model.push(entry(kind, text)));
}

fn push_image_entry(store: &slint::Weak<AgentStore<'static>>, image: cutlass_ai::ImagePart) {
    let label = transcript_image_label(&image.label);
    let frame = match decode_transcript_image(&image) {
        Ok(frame) => frame,
        Err(error) => {
            push_entry(
                store,
                "status",
                format!("Could not display image '{label}': {error}"),
            );
            return;
        }
    };
    let aspect = frame.width as f32 / frame.height as f32;
    let (width, height, pixels) = (frame.width, frame.height, frame.pixels);
    with_transcript(store, move |model| {
        let buffer =
            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, width, height);
        model.push(AgentEntry {
            kind: "image".into(),
            text: label.into(),
            image: slint::Image::from_rgba8(buffer),
            image_aspect: aspect,
        });
    });
}

fn decode_transcript_image(
    image: &cutlass_ai::ImagePart,
) -> Result<cutlass_render::RgbaImage, String> {
    const MAX_EDGE: u32 = 2_048;
    const MAX_PIXELS: u64 = 4 * 1024 * 1024;

    let frame = match image.media_type.as_str() {
        "image/png" => {
            cutlass_render::decode_png(image.data.as_slice()).map_err(|error| error.to_string())?
        }
        "image/jpeg" => cutlass_decoder::decode_image_bytes(image.data.as_slice())
            .map_err(|error| error.to_string())?,
        media_type => return Err(format!("unsupported transcript image type '{media_type}'")),
    };
    let pixels = u64::from(frame.width)
        .checked_mul(u64::from(frame.height))
        .ok_or_else(|| "transcript image dimensions overflow".to_string())?;
    if frame.width == 0
        || frame.height == 0
        || frame.width > MAX_EDGE
        || frame.height > MAX_EDGE
        || pixels > MAX_PIXELS
    {
        return Err(format!(
            "transcript image dimensions {}x{} exceed the display bound",
            frame.width, frame.height
        ));
    }
    if !frame.is_well_formed() {
        return Err("transcript image has a malformed RGBA buffer".into());
    }
    Ok(frame)
}

fn transcript_image_label(label: &str) -> String {
    const MAX_CHARS: usize = 160;
    let mut safe = String::with_capacity(label.len().min(MAX_CHARS));
    for character in label.chars().take(MAX_CHARS) {
        safe.push(if character.is_control() {
            '\u{fffd}'
        } else {
            character
        });
    }
    if label.chars().count() > MAX_CHARS {
        safe.push('…');
    }
    if safe.trim().is_empty() {
        "Agent image".to_string()
    } else {
        safe
    }
}

fn append_assistant_text(store: &slint::Weak<AgentStore<'static>>, delta: String) {
    with_transcript(store, move |model| {
        append_transcript_text(model, "assistant", delta);
    });
}

fn append_reasoning_text(store: &slint::Weak<AgentStore<'static>>, delta: String) {
    with_transcript(store, move |model| {
        append_transcript_text(model, "reasoning", delta);
    });
}

fn append_transcript_text(model: &VecModel<AgentEntry>, kind: &str, delta: String) {
    let last = model.row_count().wrapping_sub(1);
    match model.row_data(last) {
        Some(row) if row.kind == kind => {
            let mut row = row;
            row.text = format!("{}{}", row.text, delta).into();
            model.set_row_data(last, row);
        }
        _ => model.push(entry(kind, delta)),
    }
}

fn with_store(
    store: &slint::Weak<AgentStore<'static>>,
    f: impl FnOnce(AgentStore<'_>) + Send + 'static,
) {
    let store = store.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(store) = store.upgrade() {
            f(store);
        }
    });
}

/// Snapshot the visible transcript on the Slint thread. Calls are made only
/// from the dedicated agent worker; the timeout prevents shutdown from
/// hanging if the event loop has already stopped.
fn transcript_snapshot(
    store: &slint::Weak<AgentStore<'static>>,
) -> Result<Vec<TranscriptEntry>, String> {
    let (tx, rx) = bounded(1);
    let store = store.clone();
    slint::invoke_from_event_loop(move || {
        let rows = store
            .upgrade()
            .map(|store| {
                let model = store.get_transcript();
                (0..model.row_count())
                    .filter_map(|index| model.row_data(index))
                    .map(|row| TranscriptEntry {
                        kind: row.kind.to_string(),
                        text: row.text.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let _ = tx.send(rows);
    })
    .map_err(|error| format!("failed to schedule agent transcript snapshot: {error}"))?;
    rx.recv_timeout(Duration::from_secs(2))
        .map_err(|error| format!("agent transcript snapshot timed out: {error}"))
}

fn replace_transcript(
    store: &slint::Weak<AgentStore<'static>>,
    mut transcript: Vec<TranscriptEntry>,
    restore_error: Option<String>,
) {
    if let Some(error) = restore_error {
        warn!(error, "agent session could not be restored");
        transcript.push(TranscriptEntry {
            kind: "error".into(),
            text: "The previous agent conversation could not be restored.".into(),
        });
    }
    with_store(store, move |store| {
        // Build Slint image-bearing rows on the UI thread: `slint::Image`
        // is intentionally not Send even when it is empty.
        let rows: Vec<AgentEntry> = transcript
            .into_iter()
            .map(|saved| {
                if saved.kind == "image" {
                    let caption = if saved.text.is_empty() {
                        "Image attachment from the previous session.".to_string()
                    } else {
                        format!("Image attachment from the previous session: {}", saved.text)
                    };
                    entry("status", caption)
                } else {
                    entry(&saved.kind, saved.text)
                }
            })
            .collect();
        store.set_transcript(ModelRc::new(VecModel::from(rows)));
    });
}

fn persist_session(
    project: Option<&Path>,
    chat_id: Option<&str>,
    history: &[Message],
    store: &slint::Weak<AgentStore<'static>>,
) {
    let (Some(project), Some(chat_id)) = (project, chat_id) else {
        return;
    };
    let transcript = match transcript_snapshot(store) {
        Ok(transcript) => transcript,
        Err(error) => {
            warn!(error, "agent session transcript was not captured");
            return;
        }
    };
    let session = AgentSession {
        history: history.to_vec(),
        transcript,
    };
    if session.history.is_empty() && session.transcript.is_empty() {
        return;
    }
    if let Err(error) = crate::agent_session::save_chat(project, chat_id, &session) {
        warn!(
            error,
            project = %project.display(),
            chat_id,
            "agent chat was not saved"
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ChatChoice {
    id: String,
    label: String,
}

fn chat_choices(mut chats: Vec<ChatMeta>, active_chat_id: Option<&str>) -> Vec<ChatChoice> {
    if let Some(active_id) = active_chat_id {
        if !chats.iter().any(|chat| chat.id == active_id) {
            chats.insert(
                0,
                ChatMeta {
                    id: active_id.to_string(),
                    title: "New chat".to_string(),
                    updated_millis: u64::MAX,
                },
            );
        }
    }

    let mut used = HashSet::new();
    chats
        .into_iter()
        .map(|chat| {
            let base = chat.title;
            let mut label = base.clone();
            let mut suffix = 2;
            while !used.insert(label.clone()) {
                label = format!("{base} · {suffix}");
                suffix += 1;
            }
            ChatChoice { id: chat.id, label }
        })
        .collect()
}

fn publish_chat_list(
    store: &slint::Weak<AgentStore<'static>>,
    project: Option<&Path>,
    active_chat_id: Option<&str>,
) {
    let chats = match project {
        Some(project) => match crate::agent_session::list_chats(project) {
            Ok(chats) => chats,
            Err(error) => {
                warn!(error, project = %project.display(), "agent chats could not be listed");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let choices = chat_choices(chats, active_chat_id);
    let active_label = active_chat_id
        .and_then(|active_id| {
            choices
                .iter()
                .find(|choice| choice.id == active_id)
                .map(|choice| choice.label.clone())
        })
        .unwrap_or_default();
    let labels: Vec<SharedString> = choices
        .iter()
        .map(|choice| choice.label.as_str().into())
        .collect();
    let ids: Vec<SharedString> = choices.into_iter().map(|choice| choice.id.into()).collect();
    with_store(store, move |store| {
        store.set_chat_labels(ModelRc::new(VecModel::from(labels)));
        store.set_chat_ids(ModelRc::new(VecModel::from(ids)));
        store.set_active_chat_label(active_label.into());
    });
}

fn sandbox_engine() -> Result<Engine, String> {
    Engine::new(EngineConfig::default())
        .map_err(|e| format!("agent sandbox engine failed to start: {e}"))
}

trait ProjectSnapshotSource {
    fn snapshot_project(&self) -> Option<cutlass_models::Project>;
}

impl ProjectSnapshotSource for WorkerHandle {
    fn snapshot_project(&self) -> Option<cutlass_models::Project> {
        WorkerHandle::snapshot_project(self)
    }
}

struct SandboxBridge<'a, W: ProjectSnapshotSource + ?Sized> {
    worker: &'a W,
    engine: &'a mut Engine,
    plan: &'a mut Vec<AgentPlanStep>,
    senses: &'a mut AgentSenses,
    default_playhead_seconds: f64,
}

impl<W: ProjectSnapshotSource + ?Sized> EngineBridge for SandboxBridge<'_, W> {
    fn summary(&mut self) -> ProjectSummary {
        summarize(self.engine.project())
    }

    fn sense_tools(&self) -> Vec<HostToolSpec> {
        AgentSenses::specs()
    }

    fn sense(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
        cancel: &AtomicBool,
    ) -> Result<ToolOutput, String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled before the media sense could run".into());
        }
        let output = self.senses.call(
            self.engine.project(),
            self.default_playhead_seconds,
            name,
            arguments,
        )?;
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled while the media sense was running".into());
        }
        Ok(output)
    }

    fn before_host_call(
        &mut self,
        name: &str,
        _arguments: &serde_json::Value,
    ) -> Result<(), String> {
        if crate::agent_project::mutates_live_project(name) && !self.plan.is_empty() {
            return Err(format!(
                "{name} cannot run while timeline edits are staged; project operations must \
                 happen before staged edits, or after the user applies or discards the pending \
                 plan"
            ));
        }
        Ok(())
    }

    fn after_host_call(
        &mut self,
        name: &str,
        _arguments: &serde_json::Value,
        _result: Result<&ToolOutput, &str>,
    ) -> Result<(), String> {
        if !crate::agent_project::mutates_live_project(name) {
            return Ok(());
        }

        let snapshot = self.worker.snapshot_project().ok_or_else(|| {
            format!(
                "could not reconcile the agent sandbox after project host call '{name}': \
                 the editor engine did not reply with a live project snapshot"
            )
        })?;
        self.plan.clear();
        self.engine.reset_project(snapshot);
        // `reset_project` clears history, including the prompt's pending
        // group. Reopen it immediately so any later staged edit is still
        // covered by the core loop's normal abort rollback.
        self.engine.begin_group();
        Ok(())
    }

    fn apply(&mut self, command: &WireCommand) -> Result<EditOutcome, String> {
        let lowered = validate(command, self.engine.project()).map_err(|r| r.message)?;
        match self.engine.apply(lowered) {
            Ok(ApplyOutcome::Edited(outcome)) => {
                let created = match &outcome {
                    EditOutcome::Created(id) => Some(AgentCreated::Clip(id.raw())),
                    EditOutcome::CreatedTrack(id) => Some(AgentCreated::Track(id.raw())),
                    EditOutcome::CreatedMarker(id) => Some(AgentCreated::Marker(id.raw())),
                    _ => None,
                };
                self.plan.push(AgentPlanStep {
                    command: command.clone(),
                    created,
                });
                Ok(outcome)
            }
            Ok(other) => Err(format!("unexpected engine outcome: {other:?}")),
            Err(e) => Err(e.to_string()),
        }
    }

    fn check(&mut self, command: &WireCommand) -> Result<(), String> {
        validate(command, self.engine.project())
            .map(|_| ())
            .map_err(|r| r.message)
    }

    fn begin_group(&mut self) {
        self.engine.begin_group();
    }

    fn end_group(&mut self) {
        self.engine.commit_group();
    }

    fn rollback_group(&mut self) {
        self.engine.rollback_group();
    }
}

const APPROVAL_WAIT_SLICE: Duration = Duration::from_millis(50);
const APPROVAL_CARD_PUBLISH_TIMEOUT: Duration = Duration::from_secs(2);
const APPROVAL_DETAIL_MAX_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalWaitOutcome {
    Approved,
    Declined,
    Cancelled,
    ChannelClosed,
}

/// Wait for one exact approval generation. Decisions for earlier requests
/// are consumed and ignored, so they cannot leak into a later authorization.
fn wait_for_system_tool_approval(
    approval_rx: &Receiver<ApprovalDecision>,
    request_id: u64,
    cancel: &AtomicBool,
    wait_slice: Duration,
) -> ApprovalWaitOutcome {
    loop {
        if cancel.load(Ordering::Acquire) {
            return ApprovalWaitOutcome::Cancelled;
        }
        match approval_rx.recv_timeout(wait_slice) {
            Ok(decision) if decision.request_id != request_id => continue,
            Ok(decision) => {
                // Stop wins if it raced with a click that was already queued.
                if cancel.load(Ordering::Acquire) {
                    return ApprovalWaitOutcome::Cancelled;
                }
                return match decision.choice {
                    ApprovalChoice::Approve => ApprovalWaitOutcome::Approved,
                    ApprovalChoice::Deny => ApprovalWaitOutcome::Declined,
                };
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return ApprovalWaitOutcome::ChannelClosed;
            }
        }
    }
}

fn allocate_approval_request_id(allocator: &AtomicU64) -> Result<u64, String> {
    allocator
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| "system tool approval request id space is exhausted".into())
}

fn publish_approval_card(
    store: &slint::Weak<AgentStore<'static>>,
    name: &str,
    arguments: &serde_json::Value,
    cache_registry: Option<&CacheRegistry>,
    validated_import: Option<&crate::agent_project::ValidatedImportMedia>,
) -> Result<(), String> {
    let title = approval_title(name);
    let detail = approval_detail(name, arguments, cache_registry, validated_import);
    let (published_tx, published_rx) = bounded(1);
    let store = store.clone();
    slint::invoke_from_event_loop(move || {
        let published = store.upgrade().is_some_and(|store| {
            store.set_approval_title(title.into());
            store.set_approval_detail(detail.into());
            store.set_approval_pending(true);
            true
        });
        let _ = published_tx.send(published);
    })
    .map_err(|error| format!("could not show system tool approval: {error}"))?;
    match published_rx.recv_timeout(APPROVAL_CARD_PUBLISH_TIMEOUT) {
        Ok(true) => Ok(()),
        Ok(false) => Err("could not show system tool approval because the UI is closed".into()),
        Err(RecvTimeoutError::Timeout) => {
            Err("timed out while showing the system tool approval".into())
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err("system tool approval UI closed before it could be shown".into())
        }
    }
}

fn approval_title(name: &str) -> String {
    match name {
        "project_open" => "Open this project draft?".into(),
        "project_import_media" => "Import this media file?".into(),
        "system_cache_list" => "Let the assistant inspect cache usage?".into(),
        "system_cache_clear" => "Clear this cache?".into(),
        "system_cache_relocate" => "Move this cache?".into(),
        "system_reveal" => "Reveal this path?".into(),
        "system_open_external" => "Open this outside Cutlass?".into(),
        "app_close" => "Close Cutlass?".into(),
        _ => format!("Run {name}?"),
    }
}

fn approval_detail(
    name: &str,
    arguments: &serde_json::Value,
    cache_registry: Option<&CacheRegistry>,
    validated_import: Option<&crate::agent_project::ValidatedImportMedia>,
) -> String {
    if name == "project_open" {
        let draft_id = arguments
            .get("draft_id")
            .and_then(serde_json::Value::as_str)
            .filter(|draft_id| {
                !draft_id.is_empty()
                    && draft_id.chars().count() <= crate::agent_project::MAX_DRAFT_ID_CHARS
                    && draft_id.bytes().all(|byte| {
                        byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-'
                    })
            })
            .unwrap_or("<invalid draft ID>");
        return bound_approval_detail(format!(
            "Draft ID: {draft_id}\n\nOpening this draft replaces the current session and may discard unsaved work."
        ));
    }

    if name == crate::agent_project::PROJECT_IMPORT_MEDIA {
        // Normal authorization supplies an opaque validated token retained
        // for dispatch. If a future caller bypasses that flow, validate the
        // raw arguments here rather than copying hostile text into the card.
        let revalidated = crate::agent_project::validated_import_media(arguments).ok();
        let display_path = validated_import
            .or(revalidated.as_ref())
            .map(crate::agent_project::ValidatedImportMedia::canonical_path);
        let path = display_path
            .and_then(crate::agent_project::import_path_approval_display)
            .unwrap_or_else(|| "<invalid media path>".into());
        return bound_approval_detail(format!(
            "Canonical file: {path}\n\nCutlass adds a reference to this file rather than copying the source. Moving or deleting it can make the media missing."
        ));
    }

    if name == "system_cache_clear"
        && let Some(id) = arguments
            .get("cache_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|id| cutlass_storage::CacheId::parse(id).ok())
        && let Some(registry) = cache_registry
    {
        return bound_approval_detail(registry.clear_approval_detail(id));
    }

    if name == "system_cache_relocate"
        && let Some(id) = arguments
            .get("cache_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|id| cutlass_storage::CacheId::parse(id).ok())
        && let Some(destination) = arguments
            .get("destination")
            .and_then(serde_json::Value::as_str)
        && let Some(registry) = cache_registry
        && let Ok(current_path) = registry.cache_path(id)
    {
        return bound_approval_detail(format_cache_relocation_approval_detail(
            id,
            &current_path,
            Path::new(destination),
        ));
    }

    let detail = match arguments.as_object() {
        Some(arguments) if arguments.is_empty() => "No arguments.".to_string(),
        _ => serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string()),
    };
    bound_approval_detail(detail)
}

fn bound_approval_detail(detail: String) -> String {
    let mut bounded: String = detail.chars().take(APPROVAL_DETAIL_MAX_CHARS).collect();
    if detail.chars().count() > APPROVAL_DETAIL_MAX_CHARS {
        bounded.push('…');
    }
    bounded
}

fn format_cache_relocation_approval_detail(
    id: cutlass_storage::CacheId,
    current_path: &Path,
    destination: &Path,
) -> String {
    format!(
        "Cache: {}\nCurrent path: {}\nRequested destination: {}\n\nThe move may be refused when projects reference cache-owned files.",
        id.descriptor().label,
        current_path.display(),
        destination.display()
    )
}

fn clear_approval_card(store: &slint::Weak<AgentStore<'static>>) {
    with_store(store, |store| {
        store.set_approval_pending(false);
        store.set_approval_title(SharedString::default());
        store.set_approval_detail(SharedString::default());
    });
}

#[derive(Clone)]
struct DesktopToolHandles {
    store: slint::Weak<AgentStore<'static>>,
    app: slint::Weak<AppWindow>,
    cache_registry: Option<CacheRegistry>,
    job_manager: JobManager,
    worker: Option<WorkerHandle>,
}

impl DesktopToolHandles {
    fn from_runtime(runtime: &AgentRuntimeHandles, worker: &WorkerHandle) -> Self {
        Self {
            store: runtime.store.clone(),
            app: runtime.app.clone(),
            cache_registry: Some(runtime.cache_registry.clone()),
            job_manager: runtime.job_manager.clone(),
            worker: Some(worker.clone()),
        }
    }
}

struct ApprovedProjectImport {
    arguments: serde_json::Value,
    validated: crate::agent_project::ValidatedImportMedia,
}

/// The desktop host-tool surface: app and job controls plus the approval
/// broker that gates every System-tier call.
pub struct DesktopToolHost {
    autonomy: Autonomy,
    runtime: DesktopToolHandles,
    approval_rx: Receiver<ApprovalDecision>,
    pending_approval_id: Arc<AtomicU64>,
    approval_id_allocator: Arc<AtomicU64>,
    ordinary_host_call_attempted: bool,
    approved_project_import: Option<ApprovedProjectImport>,
}

impl DesktopToolHost {
    fn new(
        autonomy: Autonomy,
        runtime: DesktopToolHandles,
        approval_rx: Receiver<ApprovalDecision>,
        pending_approval_id: Arc<AtomicU64>,
        approval_id_allocator: Arc<AtomicU64>,
    ) -> Self {
        Self {
            autonomy,
            runtime,
            approval_rx,
            pending_approval_id,
            approval_id_allocator,
            ordinary_host_call_attempted: false,
            approved_project_import: None,
        }
    }

    fn ordinary_host_call_attempted(&self) -> bool {
        self.ordinary_host_call_attempted
    }
}

impl ToolHost for DesktopToolHost {
    fn tools(&self) -> Vec<HostToolSpec> {
        let mut specs = crate::agent_app_control::specs();
        specs.extend(crate::agent_project::specs());
        specs.extend(crate::agent_jobs::specs());
        specs.extend(crate::agent_system::specs());
        specs
    }

    fn authorize(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
        tier: ToolTier,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        // Authorization and dispatch are synchronous in the agent loop. Clear
        // any stale binding before considering a new call.
        self.approved_project_import = None;
        if tier != ToolTier::System || self.autonomy == Autonomy::Full {
            return Ok(());
        }
        if cancel.load(Ordering::Acquire) {
            return Err("cancelled before the system tool could run".into());
        }
        let validated_import = match cutlass_ai::namespace(name) {
            "project" => {
                crate::agent_project::validate_request(name, arguments)?;
                if name == crate::agent_project::PROJECT_IMPORT_MEDIA {
                    Some(crate::agent_project::validated_import_media(arguments)?)
                } else {
                    None
                }
            }
            "system" => {
                crate::agent_system::validate_request(name, arguments)?;
                None
            }
            _ => None,
        };
        if cancel.load(Ordering::Acquire) {
            return Err("cancelled before the system tool could run".into());
        }

        let request_id = allocate_approval_request_id(&self.approval_id_allocator)?;
        self.pending_approval_id
            .compare_exchange(0, request_id, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "another system tool approval is already pending".to_string())?;
        if let Err(error) = publish_approval_card(
            &self.runtime.store,
            name,
            arguments,
            self.runtime.cache_registry.as_ref(),
            validated_import.as_ref(),
        ) {
            let _ = self.pending_approval_id.compare_exchange(
                request_id,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            clear_approval_card(&self.runtime.store);
            return Err(error);
        }

        let outcome = wait_for_system_tool_approval(
            &self.approval_rx,
            request_id,
            cancel,
            APPROVAL_WAIT_SLICE,
        );
        let _ = self.pending_approval_id.compare_exchange(
            request_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        clear_approval_card(&self.runtime.store);

        match outcome {
            ApprovalWaitOutcome::Approved if cancel.load(Ordering::Acquire) => {
                Err("cancelled before the system tool could run".into())
            }
            ApprovalWaitOutcome::Approved => {
                if let Some(validated) = validated_import {
                    self.approved_project_import = Some(ApprovedProjectImport {
                        arguments: arguments.clone(),
                        validated,
                    });
                }
                Ok(())
            }
            ApprovalWaitOutcome::Declined => Err(format!(
                "the user declined system tool '{name}'; the tool was not run"
            )),
            ApprovalWaitOutcome::Cancelled => {
                Err("cancelled while waiting for system tool approval; the tool was not run".into())
            }
            ApprovalWaitOutcome::ChannelClosed => {
                Err("system tool approval closed; the tool was not run".into())
            }
        }
    }

    fn call(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
        cancel: &AtomicBool,
    ) -> Result<ToolOutput, String> {
        let namespace = cutlass_ai::namespace(name);
        if matches!(namespace, "app" | "system")
            || name == "job_cancel"
            || crate::agent_project::mutates_live_project(name)
        {
            // Set this before dispatch: an error can still follow a partial
            // host-side effect, so abort messaging must be conservative.
            self.ordinary_host_call_attempted = true;
        }
        let approved_import = if name == crate::agent_project::PROJECT_IMPORT_MEDIA
            && self.autonomy == Autonomy::Ask
        {
            let approval = self.approved_project_import.take().ok_or_else(|| {
                "project_import_media approval could not be confirmed; not started".to_string()
            })?;
            if approval.arguments != *arguments {
                error!("project media-import arguments changed after approval");
                return Err(
                    "project_import_media approval did not match this request; not started".into(),
                );
            }
            Some(approval.validated)
        } else {
            self.approved_project_import = None;
            None
        };
        match namespace {
            "app" => {
                crate::agent_app_control::call(self.runtime.app.clone(), name, arguments, cancel)
            }
            "project" => match approved_import.as_ref() {
                Some(approved) => crate::agent_project::call_with_approved_import(
                    self.runtime.worker.as_ref(),
                    name,
                    arguments,
                    Some(approved),
                    cancel,
                ),
                None => crate::agent_project::call(
                    self.runtime.worker.as_ref(),
                    name,
                    arguments,
                    cancel,
                ),
            },
            "job" => crate::agent_jobs::call(&self.runtime.job_manager, name, arguments, cancel),
            "system" => crate::agent_system::call(
                self.runtime.cache_registry.as_ref(),
                name,
                arguments,
                cancel,
            ),
            other => Err(format!("unsupported desktop tool namespace '{other}'")),
        }
    }
}

fn abort_status_message(reason: &str, ordinary_host_call_attempted: bool) -> String {
    if !ordinary_host_call_attempted {
        return if reason == "cancelled" {
            "Stopped — nothing was applied.".to_string()
        } else if reason.contains("402") {
            // The managed proxy's out-of-credits answer.
            "Out of Cutlass credits — buy a pack in Settings > Account. \
             Nothing was applied."
                .to_string()
        } else {
            format!("{reason} — nothing was applied.")
        };
    }

    let effect_notice = "Timeline edits staged by this prompt were rolled back and were not \
                         applied; any host actions that already completed remain in effect.";
    if reason == "cancelled" {
        format!("Stopped — {effect_notice}")
    } else if reason.contains("402") {
        format!("Out of Cutlass credits — buy a pack in Settings > Account. {effect_notice}")
    } else {
        format!("{reason} — {effect_notice}")
    }
}

#[derive(Default)]
struct Preview {
    plan: Vec<AgentPlanStep>,
    /// Phase boundaries within `plan` (exclusive step indices) from
    /// `commit_progress` — live replay commits one undo group per phase.
    phase_breaks: Vec<usize>,
    descriptions: Vec<SharedString>,
    history_restore: Option<Vec<Message>>,
}

impl Preview {
    fn is_pending(&self) -> bool {
        !self.plan.is_empty()
    }

    fn clear(&mut self) {
        self.plan.clear();
        self.phase_breaks.clear();
        self.descriptions.clear();
        self.history_restore = None;
    }
}

fn agent_main(
    worker: WorkerHandle,
    runtime: AgentRuntimeHandles,
    rx: Receiver<AgentRequest>,
    cancel: Arc<AtomicBool>,
    approval_rx: Receiver<ApprovalDecision>,
    pending_approval_id: Arc<AtomicU64>,
    approval_id_allocator: Arc<AtomicU64>,
) {
    let store = runtime.store.clone();
    let mut sandbox: Option<Engine> = None;
    let mut senses = AgentSenses::new();
    let mut preview = Preview::default();
    let mut history: Vec<Message> = Vec::new();
    let mut current_project: Option<PathBuf> = None;
    let mut current_chat_id: Option<String> = None;

    let config_path = cutlass_settings::default_config_path();
    let configured = cutlass_settings::load(&config_path)
        .map(|s| s.ai.is_configured())
        .unwrap_or(false);
    let path_text: SharedString = config_path.display().to_string().into();
    with_store(&store, move |s| {
        s.set_configured(configured);
        s.set_config_path(path_text);
    });

    while let Ok(req) = rx.recv() {
        match req {
            AgentRequest::Prompt {
                prompt,
                context,
                dry_run,
            } => {
                if current_chat_id.is_none() {
                    current_chat_id = current_project.as_deref().and_then(|project| {
                        match crate::agent_session::allocate_chat_id(project) {
                            Ok(id) => Some(id),
                            Err(error) => {
                                warn!(error, project = %project.display(), "agent chat id was not allocated");
                                None
                            }
                        }
                    });
                    publish_chat_list(
                        &store,
                        current_project.as_deref(),
                        current_chat_id.as_deref(),
                    );
                }
                cancel.store(false, Ordering::Relaxed);
                if dry_run {
                    if !preview.is_pending() {
                        preview.history_restore = Some(history.clone());
                    }
                } else if preview.is_pending() {
                    if let Some(saved) = preview.history_restore.take() {
                        history = saved;
                    }
                    preview.clear();
                    push_entry(&store, "status", "Pending preview discarded.".into());
                }
                with_store(&store, |s| {
                    s.set_running(true);
                    s.set_plan_pending(false);
                    s.set_undo_offered(false);
                });
                push_entry(&store, "user", prompt.clone());

                // Reload ~/.cutlass/agent every prompt (tiny files) so
                // rule/skill/command edits apply without a restart.
                let agent_dir = load_agent_dir(&cutlass_settings::agent_dir());
                for warning in &agent_dir.warnings {
                    warn!(warning, "agent extension file skipped");
                    push_entry(&store, "status", warning.clone());
                }
                // Slash commands expand client-side; the transcript keeps
                // what was typed, the model sees the template.
                let sent = match expand_slash_command(&prompt, &agent_dir.commands) {
                    Some(expanded) => {
                        let name = prompt[1..].split_whitespace().next().unwrap_or("");
                        push_entry(&store, "status", format!("Expanded /{name}."));
                        expanded
                    }
                    None => prompt.clone(),
                };

                run_one_prompt(
                    &worker,
                    &runtime,
                    &mut sandbox,
                    &mut senses,
                    &mut preview,
                    &mut history,
                    &sent,
                    context,
                    agent_dir,
                    dry_run,
                    &cancel,
                    &approval_rx,
                    &pending_approval_id,
                    &approval_id_allocator,
                );

                with_store(&store, |s| s.set_running(false));
                persist_session(
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                    &history,
                    &store,
                );
                publish_chat_list(
                    &store,
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                );
            }
            AgentRequest::ApplyPlan => {
                let plan = std::mem::take(&mut preview.plan);
                let phase_breaks = std::mem::take(&mut preview.phase_breaks);
                preview.clear();
                with_store(&store, |s| s.set_plan_pending(false));
                if plan.is_empty() {
                    continue;
                }
                apply_plan_live(&worker, &store, plan, &phase_breaks);
                persist_session(
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                    &history,
                    &store,
                );
                publish_chat_list(
                    &store,
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                );
            }
            AgentRequest::DiscardPlan => {
                if preview.is_pending() {
                    if let Some(saved) = preview.history_restore.take() {
                        history = saved;
                    }
                    preview.clear();
                    push_entry(
                        &store,
                        "status",
                        "Plan discarded — nothing was applied.".into(),
                    );
                }
                with_store(&store, |s| s.set_plan_pending(false));
                persist_session(
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                    &history,
                    &store,
                );
                publish_chat_list(
                    &store,
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                );
            }
            AgentRequest::NewChat => {
                if let Some(saved) = preview.history_restore.take() {
                    history = saved;
                }
                preview.clear();
                with_store(&store, |s| s.set_plan_pending(false));
                persist_session(
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                    &history,
                    &store,
                );

                current_chat_id = current_project.as_deref().and_then(|project| {
                    match crate::agent_session::allocate_chat_id(project) {
                        Ok(id) => Some(id),
                        Err(error) => {
                            warn!(error, project = %project.display(), "agent chat id was not allocated");
                            None
                        }
                    }
                });
                history.clear();
                replace_transcript(&store, Vec::new(), None);
                publish_chat_list(
                    &store,
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                );
            }
            AgentRequest::SelectChat { id } => {
                if current_chat_id.as_deref() == Some(id.as_str()) {
                    publish_chat_list(
                        &store,
                        current_project.as_deref(),
                        current_chat_id.as_deref(),
                    );
                    continue;
                }
                if let Some(saved) = preview.history_restore.take() {
                    history = saved;
                }
                preview.clear();
                with_store(&store, |s| s.set_plan_pending(false));
                persist_session(
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                    &history,
                    &store,
                );

                let loaded = current_project
                    .as_deref()
                    .ok_or_else(|| "no project is open".to_string())
                    .and_then(|project| crate::agent_session::load_chat(project, &id));
                match loaded {
                    Ok(session) => {
                        history = session.history;
                        replace_transcript(&store, session.transcript, None);
                        current_chat_id = Some(id);
                    }
                    Err(error) => {
                        warn!(error, chat_id = id, "agent chat could not be restored");
                        push_entry(
                            &store,
                            "error",
                            "That chat could not be restored.".to_string(),
                        );
                    }
                }
                publish_chat_list(
                    &store,
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                );
            }
            AgentRequest::SwitchProject { path } => {
                // A parked dry-run conversation names edits that never
                // landed. Restore the history checkpoint before persisting
                // the draft we are leaving.
                if let Some(saved) = preview.history_restore.take() {
                    history = saved;
                }
                preview.clear();
                with_store(&store, |s| s.set_plan_pending(false));
                persist_session(
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                    &history,
                    &store,
                );

                let (next_chat_id, session, restore_error) = match path.as_deref() {
                    Some(project) => match crate::agent_session::list_chats(project) {
                        Ok(chats) => match chats.first() {
                            Some(chat) => {
                                match crate::agent_session::load_chat(project, &chat.id) {
                                    Ok(session) => (Some(chat.id.clone()), session, None),
                                    Err(error) => (
                                        Some(chat.id.clone()),
                                        AgentSession::default(),
                                        Some(error),
                                    ),
                                }
                            }
                            None => match crate::agent_session::allocate_chat_id(project) {
                                Ok(id) => (Some(id), AgentSession::default(), None),
                                Err(error) => (None, AgentSession::default(), Some(error)),
                            },
                        },
                        Err(error) => match crate::agent_session::allocate_chat_id(project) {
                            Ok(id) => (Some(id), AgentSession::default(), Some(error)),
                            Err(allocation_error) => (
                                None,
                                AgentSession::default(),
                                Some(format!("{error}; {allocation_error}")),
                            ),
                        },
                    },
                    None => (None, AgentSession::default(), None),
                };
                history = session.history;
                replace_transcript(&store, session.transcript, restore_error);
                current_project = path;
                current_chat_id = next_chat_id;
                publish_chat_list(
                    &store,
                    current_project.as_deref(),
                    current_chat_id.as_deref(),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_one_prompt(
    worker: &WorkerHandle,
    runtime: &AgentRuntimeHandles,
    sandbox: &mut Option<Engine>,
    senses: &mut AgentSenses,
    preview: &mut Preview,
    history: &mut Vec<Message>,
    prompt: &str,
    context: EditorContext,
    agent_dir: cutlass_ai::AgentDir,
    dry_run: bool,
    cancel: &AtomicBool,
    approval_rx: &Receiver<ApprovalDecision>,
    pending_approval_id: &Arc<AtomicU64>,
    approval_id_allocator: &Arc<AtomicU64>,
) {
    let store = &runtime.store;
    let config_path = cutlass_settings::default_config_path();
    let section = match cutlass_settings::load(&config_path) {
        Ok(settings) => settings.ai,
        Err(e) => {
            push_entry(store, "error", e);
            return;
        }
    };
    if !section.is_configured() {
        with_store(store, |s| s.set_configured(false));
        push_entry(
            store,
            "error",
            format!(
                "No AI provider configured. Add an endpoint and model in \
                 Settings (or an [ai] table in {}), then send again.",
                config_path.display()
            ),
        );
        return;
    }
    // The third provider mode: "Cutlass account" routes through the
    // backend's OpenAI-compatible managed proxy with the keychain session
    // as the bearer (model pinned server-side; credits metered there).
    let provider = if section.use_account {
        let token = match crate::account::managed_access_token() {
            Ok(token) => token,
            Err(e) => {
                push_entry(store, "error", e);
                return;
            }
        };
        with_store(store, |s| s.set_configured(true));
        OpenAiProvider::new(
            &format!("{}/v1/generate", crate::account::base_url()),
            "cutlass-managed",
            Some(token),
            OpenAiProtocol::ChatCompletions,
            ReasoningSummary::Off,
        )
    } else {
        let api_key = match cutlass_ai::config::resolve_api_key(
            section.api_key.as_deref(),
            section.api_key_env.as_deref(),
        ) {
            Ok(key) => key,
            Err(e) => {
                push_entry(store, "error", e);
                return;
            }
        };
        with_store(store, |s| s.set_configured(true));
        let protocol = match section.api_protocol {
            cutlass_settings::AiApiProtocol::ChatCompletions => OpenAiProtocol::ChatCompletions,
            cutlass_settings::AiApiProtocol::Responses => OpenAiProtocol::Responses,
        };
        let reasoning_summary = match section.reasoning_summary {
            cutlass_settings::ReasoningSummary::Auto => ReasoningSummary::Auto,
            cutlass_settings::ReasoningSummary::Off => ReasoningSummary::Off,
        };
        OpenAiProvider::new(
            &section.base_url,
            &section.model,
            api_key,
            protocol,
            reasoning_summary,
        )
    };

    let sandbox_existed = sandbox.is_some();
    let engine = match sandbox {
        Some(engine) => engine,
        None => match sandbox_engine() {
            Ok(engine) => sandbox.insert(engine),
            Err(e) => {
                push_entry(store, "error", e);
                return;
            }
        },
    };

    let continue_pending = preview.is_pending() && sandbox_existed;
    if !continue_pending {
        let Some(snapshot) = worker.snapshot_project() else {
            push_entry(
                store,
                "error",
                "The editor engine is not responding.".into(),
            );
            return;
        };
        engine.reset_project(snapshot);
        preview.plan.clear();
        preview.phase_breaks.clear();
        preview.descriptions.clear();
    }

    // Compose rules after the snapshot reset so per-project rules read
    // from the project this prompt actually edits (imported projects
    // included — the panel shows them via EditorStore.project.agent-rules).
    let mut sections: Vec<(String, String)> = agent_dir
        .rules
        .into_iter()
        .map(|(stem, text)| (format!("user rule: {stem}"), text))
        .collect();
    let project_rules = engine.project().metadata().agent_rules.clone();
    if !project_rules.trim().is_empty() {
        sections.push(("project rules".into(), project_rules));
    }
    let (rules, truncated) = compose_rules(&sections);
    with_store(store, move |s| s.set_rules_truncated(truncated));
    if truncated {
        push_entry(
            store,
            "status",
            "Rules exceed the size cap and were truncated.".into(),
        );
    }
    let extensions = AgentExtensions {
        rules,
        skills: merge_skills(agent_dir.skills),
    };

    // A continued preview accumulates steps; the outcome's phase breaks
    // index this prompt's actions only, so offset them into the plan.
    let plan_base = preview.plan.len();
    let mut plan: Vec<AgentPlanStep> = preview.plan.clone();
    let mut bridge = SandboxBridge {
        worker,
        engine,
        plan: &mut plan,
        senses,
        default_playhead_seconds: context.playhead_seconds,
    };
    let mut tool_host = DesktopToolHost::new(
        section.autonomy,
        DesktopToolHandles::from_runtime(runtime, worker),
        approval_rx.clone(),
        pending_approval_id.clone(),
        approval_id_allocator.clone(),
    );
    let event_store = store.clone();
    let mut on_event = move |event: AgentEvent| match event {
        AgentEvent::TextDelta(delta) => append_assistant_text(&event_store, delta),
        AgentEvent::ReasoningDelta(delta) => append_reasoning_text(&event_store, delta),
        AgentEvent::Action(action) => push_entry(&event_store, "action", action.description),
        AgentEvent::HostAction { name, summary } => {
            push_entry(&event_store, "action", format!("{name}: {summary}"))
        }
        AgentEvent::Image(image) => push_image_entry(&event_store, image),
    };

    info!(prompt, dry_run, "agent prompt started");
    let outcome = run_prompt_with_host(
        &provider,
        &mut bridge,
        &mut tool_host,
        &context,
        &extensions,
        history,
        prompt,
        &AgentConfig::default(),
        cancel,
        &mut on_event,
    );
    let ordinary_host_call_attempted = tool_host.ordinary_host_call_attempted();

    match outcome.status {
        PromptStatus::Aborted(reason) => {
            warn!(reason, "agent prompt aborted");
            push_entry(
                store,
                "error",
                abort_status_message(&reason, ordinary_host_call_attempted),
            );
        }
        PromptStatus::Completed | PromptStatus::DryRun => {
            info!(actions = plan.len(), "agent prompt completed");
            history.extend(outcome.turn_messages);
            trim_history(history);
            if dry_run {
                preview.plan = plan;
                preview
                    .phase_breaks
                    .extend(outcome.phase_breaks.iter().map(|b| plan_base + b));
                preview.descriptions.extend(
                    outcome
                        .actions
                        .iter()
                        .map(|a| SharedString::from(a.description.clone())),
                );
            } else if !plan.is_empty() {
                // Auto-apply never extends a parked preview (any pending one
                // was discarded above), so the breaks are plan-relative.
                apply_plan_live(worker, store, plan, &outcome.phase_breaks);
            }
        }
    }

    let pending = preview.is_pending();
    let descriptions = preview.descriptions.clone();
    with_store(store, move |s| {
        if pending {
            s.set_plan_actions(ModelRc::new(VecModel::from(descriptions)));
        }
        s.set_plan_pending(pending);
    });
}

/// Split a rehearsed plan at its phase breaks (exclusive step indices,
/// strictly increasing). The remainder past the last break is the final
/// phase; a break flush with the plan's end leaves no empty group behind.
fn split_plan_phases(mut plan: Vec<AgentPlanStep>, breaks: &[usize]) -> Vec<Vec<AgentPlanStep>> {
    let mut phases = Vec::with_capacity(breaks.len() + 1);
    for &at in breaks.iter().rev() {
        if at < plan.len() {
            phases.push(plan.split_off(at));
        }
    }
    if !plan.is_empty() {
        phases.push(plan);
    }
    phases.reverse();
    phases
}

fn apply_plan_live(
    worker: &WorkerHandle,
    store: &slint::Weak<AgentStore<'static>>,
    plan: Vec<AgentPlanStep>,
    phase_breaks: &[usize],
) {
    let count = plan.len();
    let phases = split_plan_phases(plan, phase_breaks);
    let phase_count = phases.len();
    match worker.agent_apply_plan(phases) {
        Some(Ok(())) => {
            push_entry(
                store,
                "applied",
                if phase_count > 1 {
                    format!("Applied {count} edits in {phase_count} undo steps.")
                } else {
                    format!(
                        "Applied {count} edit{} as one undo step.",
                        if count == 1 { "" } else { "s" }
                    )
                },
            );
            with_store(store, |s| s.set_undo_offered(true));
        }
        Some(Err(e)) => {
            error!(error = e, "agent plan replay failed");
            // The replay error already says how much (if anything) landed.
            push_entry(store, "error", format!("Could not apply the plan: {e}."));
        }
        None => push_entry(
            store,
            "error",
            "The editor engine is not responding.".into(),
        ),
    }
}

const HISTORY_CHAR_BUDGET: usize = 24_000;

fn trim_history(history: &mut Vec<Message>) {
    while history_chars(history) > HISTORY_CHAR_BUDGET {
        let next_turn = history
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, m)| matches!(m, Message::User { .. }))
            .map(|(i, _)| i);
        match next_turn {
            Some(i) => {
                history.drain(0..i);
            }
            None => break,
        }
    }
}

fn history_chars(history: &[Message]) -> usize {
    history.iter().map(message_chars).sum()
}

fn message_chars(m: &Message) -> usize {
    match m {
        Message::System { content } => content.len(),
        // History is text-only (turns strip images to labeled placeholders
        // before they land here), but count any payload that does appear so
        // the budget can never be blown by raw image bytes.
        Message::User { content, images } => {
            content.len() + images.iter().map(|i| i.data.len()).sum::<usize>()
        }
        Message::Assistant {
            content,
            tool_calls,
        } => {
            content.len()
                + tool_calls
                    .iter()
                    .map(|c| c.name.len() + c.arguments.to_string().len())
                    .sum::<usize>()
        }
        Message::ToolResult {
            content, images, ..
        } => content.len() + images.iter().map(|i| i.data.len()).sum::<usize>(),
    }
}

#[cfg(test)]
mod tests;
