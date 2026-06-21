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
//! Preview is cumulative: a follow-up prompt builds on the still-parked
//! plan rather than discarding it — the sandbox keeps those edits applied,
//! so the model's memory and the state it reads stay in sync, and the
//! combined plan grows until the user applies or discards. Discard restores
//! the conversation to before the preview began, so the model never carries
//! memory of edits that were thrown away.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, unbounded};
use cutlass_ai::providers::openai_compat::OpenAiCompatProvider;
use cutlass_ai::{
    AgentConfig, AgentEvent, EditorContext, EngineBridge, Message, ProjectSummary, PromptStatus,
    WireCommand, run_prompt, summarize, validate,
};
use cutlass_commands::EditOutcome;
use cutlass_engine::{ApplyOutcome, Engine, EngineConfig};
use slint::{Model, ModelRc, SharedString, VecModel};
use tracing::{error, info, warn};

use crate::preview_worker::WorkerHandle;
use crate::{AgentEntry, AgentStore};

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
    /// Forget the conversation so far (the project was replaced wholesale —
    /// open / new / restore — and prior turns name clips that are gone).
    ResetHistory,
}

#[derive(Clone)]
pub struct AgentHandle {
    tx: Sender<AgentRequest>,
    cancel: Arc<AtomicBool>,
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

    /// Drop the multi-turn conversation memory; the next prompt starts a
    /// fresh dialogue. Fired when the project is replaced wholesale.
    pub fn reset_history(&self) {
        let _ = self.tx.send(AgentRequest::ResetHistory);
    }

    /// Cooperative cancel: the provider checks this flag between stream
    /// chunks, so a running prompt aborts within one network read.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct AgentWorker {
    handle: AgentHandle,
    _join: JoinHandle<()>,
}

impl AgentWorker {
    pub fn spawn(
        worker: WorkerHandle,
        store: slint::Weak<AgentStore<'static>>,
    ) -> Result<Self, String> {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let join = std::thread::Builder::new()
            .name("cutlass-agent".into())
            .spawn(move || agent_main(worker, store, rx, thread_cancel))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            handle: AgentHandle { tx, cancel },
            _join: join,
        })
    }

    pub fn handle(&self) -> AgentHandle {
        self.handle.clone()
    }
}

// --- transcript publishing ------------------------------------------------

/// Entry kinds the panel styles: "user", "assistant", "action", "status",
/// "applied", "error".
fn entry(kind: &str, text: impl Into<SharedString>) -> AgentEntry {
    AgentEntry {
        kind: kind.into(),
        text: text.into(),
    }
}

/// Run `f` against the transcript's `VecModel` on the UI thread.
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

/// Append streamed text to the trailing assistant entry, creating it if the
/// last entry isn't one (first delta of a turn, or text after actions).
fn append_assistant_text(store: &slint::Weak<AgentStore<'static>>, delta: String) {
    with_transcript(store, move |model| {
        let last = model.row_count().wrapping_sub(1);
        match model.row_data(last) {
            Some(e) if e.kind == "assistant" => {
                let mut e = e;
                e.text = format!("{}{}", e.text, delta).into();
                model.set_row_data(last, e);
            }
            _ => model.push(entry("assistant", delta)),
        }
    });
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

// --- sandbox ----------------------------------------------------------------

/// The agent's rehearsal engine: real dispatch semantics (links, lane
/// creation, ripple) against a project snapshot, no UI, no preview. Cache
/// and GPU exist because `Engine` owns them, but nothing renders.
fn sandbox_engine() -> Result<Engine, String> {
    let config = EngineConfig {
        cache_dir: std::env::temp_dir().join("cutlass-agent-sandbox"),
        // The sandbox never decodes a frame; keep the budget token-sized.
        cache_budget_bytes: 16 * 1024 * 1024,
        ..EngineConfig::default()
    };
    Engine::new(config).map_err(|e| format!("agent sandbox engine failed to start: {e}"))
}

struct SandboxBridge<'a> {
    engine: &'a mut Engine,
    plan: &'a mut Vec<AgentPlanStep>,
}

impl EngineBridge for SandboxBridge<'_> {
    fn summary(&mut self) -> ProjectSummary {
        summarize(self.engine.project())
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

// --- pending preview --------------------------------------------------------

/// A rehearsed-but-unapplied plan, parked while the user decides (preview
/// mode). Follow-up prompts build on it cumulatively: the sandbox keeps the
/// plan applied so the model's memory and the state it sees stay in sync,
/// and the combined plan grows until the user applies or discards it.
#[derive(Default)]
struct Preview {
    /// Every rehearsed step, in order, ready for one live replay.
    plan: Vec<AgentPlanStep>,
    /// Editor-language line per step, for the Apply/Discard card.
    descriptions: Vec<SharedString>,
    /// The conversation as it stood before this preview session began.
    /// Restored verbatim on Discard so the model never "remembers" edits
    /// that were thrown away (the divergence that otherwise makes it
    /// distrust the next snapshot).
    history_restore: Option<Vec<Message>>,
}

impl Preview {
    fn is_pending(&self) -> bool {
        !self.plan.is_empty()
    }

    fn clear(&mut self) {
        self.plan.clear();
        self.descriptions.clear();
        self.history_restore = None;
    }
}

// --- the worker -------------------------------------------------------------

fn agent_main(
    worker: WorkerHandle,
    store: slint::Weak<AgentStore<'static>>,
    rx: Receiver<AgentRequest>,
    cancel: Arc<AtomicBool>,
) {
    // Lazy: most sessions never open the assistant, and `Engine::new`
    // spins a headless GPU context we shouldn't pay for at launch.
    let mut sandbox: Option<Engine> = None;
    // The parked, rehearsed-but-unapplied plan. Follow-up prompts extend
    // it (cumulative preview) until the user applies or discards.
    let mut preview = Preview::default();
    // Multi-turn memory: each completed prompt appends its turn here, and
    // the next prompt sees it (behind a freshly regenerated system prompt,
    // so current project state always wins). Trimmed to a budget; reset
    // when the project is replaced.
    let mut history: Vec<Message> = Vec::new();

    // Surface the configured/not-configured state before the first send.
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
                cancel.store(false, Ordering::Relaxed);
                if dry_run {
                    // Starting (or continuing) a preview session. Capture
                    // the conversation as it is now the first time, so a
                    // later Discard can restore it; follow-up prompts build
                    // on the pending plan instead of discarding it.
                    if !preview.is_pending() {
                        preview.history_restore = Some(history.clone());
                    }
                } else if preview.is_pending() {
                    // Preview turned off with a plan still parked: drop the
                    // un-applied proposal and the memory of it, then run
                    // fresh and apply immediately.
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

                run_one_prompt(
                    &worker,
                    &store,
                    &mut sandbox,
                    &mut preview,
                    &mut history,
                    &prompt,
                    context,
                    dry_run,
                    &cancel,
                );

                with_store(&store, |s| s.set_running(false));
            }
            AgentRequest::ApplyPlan => {
                let plan = std::mem::take(&mut preview.plan);
                preview.clear();
                with_store(&store, |s| s.set_plan_pending(false));
                if plan.is_empty() {
                    continue;
                }
                apply_plan_live(&worker, &store, plan);
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
            }
            AgentRequest::ResetHistory => {
                history.clear();
                preview.clear();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_one_prompt(
    worker: &WorkerHandle,
    store: &slint::Weak<AgentStore<'static>>,
    sandbox: &mut Option<Engine>,
    preview: &mut Preview,
    history: &mut Vec<Message>,
    prompt: &str,
    context: EditorContext,
    dry_run: bool,
    cancel: &AtomicBool,
) {
    // Config is re-read per prompt so edits made in Settings (or by hand to
    // config.toml) apply without an app restart (the file is tiny; this is a
    // cold path).
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
    let provider = OpenAiCompatProvider::new(&section.base_url, &section.model, api_key);

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

    // Continue building on a still-pending preview so the model's memory
    // and the state it sees agree (the sandbox already holds those edits);
    // otherwise rehearse against a fresh snapshot of the live project.
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
        // A parked plan we can't rebuild on (the sandbox was just created)
        // is dropped rather than replayed against the wrong base.
        preview.plan.clear();
        preview.descriptions.clear();
    }

    // The combined plan starts from whatever is already parked; the bridge
    // appends this turn's steps onto it.
    let mut plan: Vec<AgentPlanStep> = preview.plan.clone();
    let mut bridge = SandboxBridge {
        engine,
        plan: &mut plan,
    };
    let event_store = store.clone();
    let mut on_event = move |event: AgentEvent| match event {
        AgentEvent::TextDelta(delta) => append_assistant_text(&event_store, delta),
        AgentEvent::Action(action) => push_entry(&event_store, "action", action.description),
    };

    info!(prompt, dry_run, "agent prompt started");
    let outcome = run_prompt(
        &provider,
        &mut bridge,
        &context,
        history,
        prompt,
        &AgentConfig::default(),
        cancel,
        &mut on_event,
    );

    match outcome.status {
        PromptStatus::Aborted(reason) => {
            warn!(reason, "agent prompt aborted");
            push_entry(
                store,
                "error",
                if reason == "cancelled" {
                    "Stopped — nothing was applied.".to_string()
                } else {
                    format!("{reason} — nothing was applied.")
                },
            );
        }
        // The sandbox loop always runs with the agent's dry_run off, so
        // Completed is the only success status; the UI-level dry-run choice
        // decides what happens to the rehearsed plan next.
        PromptStatus::Completed | PromptStatus::DryRun => {
            info!(actions = plan.len(), "agent prompt completed");
            // Remember this turn even when it made no edits (Q&A is the case
            // that most needs memory).
            history.extend(outcome.turn_messages);
            trim_history(history);
            if dry_run {
                // Park the combined plan (prior steps + this turn's) and its
                // descriptions for the Apply/Discard card.
                preview.plan = plan;
                preview.descriptions.extend(
                    outcome
                        .actions
                        .iter()
                        .map(|a| SharedString::from(a.description.clone())),
                );
            } else if !plan.is_empty() {
                apply_plan_live(worker, store, plan);
            }
        }
    }

    // Reflect the parked preview in the card: grown this turn, unchanged
    // after a pure-question turn, still intact after an abort (the sandbox
    // rolled back only this turn's group), or gone after an apply.
    let pending = preview.is_pending();
    let descriptions = preview.descriptions.clone();
    with_store(store, move |s| {
        if pending {
            s.set_plan_actions(ModelRc::new(VecModel::from(descriptions)));
        }
        s.set_plan_pending(pending);
    });
}

/// Replay a rehearsed plan on the live engine (one history group, one
/// undo). Failure means the project changed since the rehearsal — the
/// whole plan rolls back and the transcript says why.
fn apply_plan_live(
    worker: &WorkerHandle,
    store: &slint::Weak<AgentStore<'static>>,
    plan: Vec<AgentPlanStep>,
) {
    let count = plan.len();
    match worker.agent_apply_plan(plan) {
        Some(Ok(())) => {
            push_entry(
                store,
                "applied",
                format!(
                    "Applied {count} edit{} as one undo step.",
                    if count == 1 { "" } else { "s" }
                ),
            );
            with_store(store, |s| s.set_undo_offered(true));
        }
        Some(Err(e)) => {
            error!(error = e, "agent plan replay failed");
            push_entry(
                store,
                "error",
                format!("Could not apply the plan: {e}. Nothing was changed."),
            );
        }
        None => push_entry(
            store,
            "error",
            "The editor engine is not responding.".into(),
        ),
    }
}

// --- session memory ---------------------------------------------------------

/// Soft ceiling on the conversation carried into each prompt, in characters
/// of message content. A turn that splits a clip and reports back is a few
/// hundred chars, so this holds dozens of turns; `describe_project` blobs
/// are collapsed upstream so they don't blow it.
const HISTORY_CHAR_BUDGET: usize = 24_000;

/// Keep history bounded: drop oldest whole turns (each begins with a `User`
/// message) until under budget. One turn is always kept — even alone it is
/// useful, and the fresh system prompt carries current state regardless.
fn trim_history(history: &mut Vec<Message>) {
    while history_chars(history) > HISTORY_CHAR_BUDGET {
        let next_turn = history
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, m)| matches!(m, Message::User { .. }))
            .map(|(i, _)| i);
        match next_turn {
            // Drop from the front up to the next turn boundary, so a
            // turn's tool-call/result pairs are never split.
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
        Message::System { content } | Message::User { content } => content.len(),
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
        Message::ToolResult { content, .. } => content.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview_worker::agent_replay;
    use cutlass_ai::wire;
    use cutlass_models::{MediaSource, Project, Rational};

    fn fixture_project() -> (Project, u64) {
        let mut project = Project::new("agent-ui-fixture", Rational::FPS_24);
        let media = project
            .add_media(MediaSource::new(
                "/tmp/agent-ui-fixture.mp4",
                1920,
                1080,
                Rational::FPS_24,
                60 * 24,
                false,
            ))
            .raw();
        (project, media)
    }

    fn temp_engine(project: Project) -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = EngineConfig {
            cache_dir: dir.path().join("cache"),
            cache_budget_bytes: 16 * 1024 * 1024,
            ..EngineConfig::default()
        };
        let engine = Engine::with_project(config, project).expect("engine");
        (dir, engine)
    }

    /// The full sandbox → live path: rehearse a prompt's worth of edits
    /// (creating a track and splitting a clip — both allocate ids), then
    /// replay on a second engine. Ids are process-global atomics, so the
    /// live engine hands out different ones than the sandbox did; the
    /// later steps that reference them only succeed if the replay's
    /// remapping works. One undo unwinds the whole plan.
    #[test]
    fn rehearsed_plan_replays_with_id_remapping_and_single_undo() {
        let (project, media) = fixture_project();
        let (_d1, mut sandbox) = temp_engine(project.clone());
        let (_d2, mut live) = temp_engine(project);

        let mut plan: Vec<AgentPlanStep> = Vec::new();
        let mut bridge = SandboxBridge {
            engine: &mut sandbox,
            plan: &mut plan,
        };
        bridge.begin_group();
        let track = match bridge
            .apply(&WireCommand::AddTrack(wire::AddTrack {
                kind: wire::WireTrackKind::Video,
                name: "V1".into(),
                index: None,
            }))
            .expect("add track")
        {
            EditOutcome::CreatedTrack(id) => id.raw(),
            other => panic!("expected created track, got {other:?}"),
        };
        let head = match bridge
            .apply(&WireCommand::AddClip(wire::AddClip {
                track,
                media,
                source_start: 0.0,
                source_duration: 10.0,
                start: 0.0,
            }))
            .expect("add clip")
        {
            EditOutcome::Created(id) => id.raw(),
            other => panic!("expected created clip, got {other:?}"),
        };
        let right = match bridge
            .apply(&WireCommand::SplitClip(wire::SplitClip {
                clip: head,
                at: 4.0,
            }))
            .expect("split clip")
        {
            EditOutcome::Created(id) => id.raw(),
            other => panic!("expected created clip, got {other:?}"),
        };
        bridge
            .apply(&WireCommand::TrimClip(wire::TrimClip {
                clip: right,
                start: 4.0,
                duration: 2.0,
            }))
            .expect("trim clip");
        bridge.end_group();
        assert_eq!(plan.len(), 4);

        agent_replay(&mut live, plan, |_| {}).expect("replay");

        let timeline = live.project().timeline();
        assert_eq!(timeline.track_count(), 1);
        assert_eq!(timeline.clip_count(), 2);

        assert!(live.undo(), "the plan is one undo entry");
        assert_eq!(live.project().timeline().track_count(), 0);
        assert!(!live.undo(), "nothing left to undo");
    }

    /// A plan rehearsed against a stale snapshot (the clip it trims no
    /// longer exists) must fail loudly and leave the live project intact.
    #[test]
    fn stale_plan_rolls_back_and_reports() {
        let (project, _media) = fixture_project();
        let (_dir, mut live) = temp_engine(project);

        let plan = vec![AgentPlanStep {
            command: WireCommand::TrimClip(wire::TrimClip {
                clip: 999_999,
                start: 0.0,
                duration: 1.0,
            }),
            created: None,
        }];
        let err = agent_replay(&mut live, plan, |_| {}).expect_err("stale plan must fail");
        assert!(err.contains("step 1/1"), "names the failing step: {err}");
        assert!(!live.undo(), "rollback leaves no history entry");
    }
}
