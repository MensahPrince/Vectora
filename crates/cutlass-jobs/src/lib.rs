//! Background-job registry: every "run this on a thread, stream progress,
//! allow cancel, show status" need in the app goes through one substrate.
//!
//! Exports, media analysis, transcription, and Python scripts all share a
//! lifecycle — spawn, report progress, maybe get cancelled, land in a
//! terminal state — and each was about to grow its own thread + flag +
//! channel plumbing. [`JobManager`] is the shared registry instead: the app
//! creates one, every subsystem spawns through a clone, the UI renders
//! [`JobSnapshot`]s, and the AI agent later gets `job_list` / `job_status` /
//! `job_cancel` tools over the same surface for free.
//!
//! Design rules:
//!
//! - **std-only.** Named threads (`std::thread::Builder`), an `Arc<Mutex<_>>`
//!   registry, atomics for ids and cancel flags. No async runtime — this is
//!   a std-thread app, and a job is exactly one thread.
//! - **Cancellation is cooperative, with an ordered commit boundary.** During
//!   cancellable preparation, [`JobManager::cancel`] sets a flag observed via
//!   [`JobContext::cancelled`] or an owned [`CancellationToken`]. Immediately
//!   before its first irreversible publication step, work calls
//!   [`JobContext::try_begin_commit`]. The request and boundary contend under
//!   the registry mutex: if cancellation wins, the boundary returns `false`
//!   and publication must be skipped; if the boundary wins, later requests
//!   are rejected and the closure's result decides Done/Failed. Before that
//!   boundary, an accepted cancellation makes either `Ok` or `Err` terminally
//!   [`JobState::Cancelled`] (panic remains Failed), even if work never checks
//!   the flag.
//! - **Subscribers are called with no lock held and isolated from each
//!   other.** Every transition collects the snapshot and subscriber list
//!   under the lock, drops the guard, then calls — so callbacks may re-enter
//!   the registry (query, spawn, cancel) without deadlocking. Each callback
//!   is separately panic-contained, so one broken UI bridge cannot alter a
//!   job's lifecycle or keep healthy subscribers from receiving the event.
//!   The `Queued` notification runs on the spawning thread, everything after
//!   on the job's own thread; because `Queued` is delivered before the thread
//!   starts, a subscriber sees each job strictly in lifecycle order: Queued
//!   → Running → progress/boundary… → terminal.
//! - **`Instant`, not `SystemTime`.** Timestamps exist to render elapsed
//!   time and to order pruning — both monotonic concerns; wall-clock time
//!   can jump under NTP. Nothing here persists across runs, so wall time
//!   buys nothing.
//! - **Bounded.** Labels and details are sanitized to fixed byte caps, and
//!   successful structured outputs are validated against deliberately small
//!   count/name/value caps. Finished jobs beyond the newest [`KEEP_FINISHED`]
//!   are pruned (by *finish* time — a long export that just ended is recent
//!   news even if it was spawned first). Running jobs are never pruned.
//! - **Panic containment.** A panicking job is caught (`catch_unwind`) and
//!   becomes [`JobState::Failed`] with a `panicked: …` detail — one bad
//!   export must not take down the app. Panic wins over cancellation: a job
//!   that blows up on the way out is a bug to surface, not a clean stop.

use std::any::Any;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

/// How many finished (Done / Failed / Cancelled) jobs the registry keeps.
/// The oldest-finished beyond this are dropped at each terminal transition,
/// so the registry stays a bounded status board, never an unbounded history.
pub const KEEP_FINISHED: usize = 64;

/// Maximum UTF-8 byte length retained for a job label.
///
/// Longer labels are truncated on a character boundary and end in one
/// ellipsis within this cap. Controls and line separators are replaced with
/// spaces before labels reach snapshots, thread names, or UI/tool boundaries.
pub const MAX_JOB_LABEL_BYTES: usize = 256;

/// Maximum UTF-8 byte length retained for progress and terminal details.
///
/// Longer details are truncated on a character boundary and end in one
/// ellipsis within this cap. Controls and line separators are replaced with
/// spaces so every retained detail is safe to render as one line.
pub const MAX_JOB_DETAIL_BYTES: usize = 4096;

/// Maximum number of structured outputs retained for one successful job.
///
/// Job-list consumers may read many jobs at once, so this count is a
/// performance and response-size bound rather than a serialization detail.
pub const MAX_JOB_OUTPUTS: usize = 8;

/// Maximum ASCII byte length of a structured output name.
///
/// Names must also match `[a-z][a-z0-9_]*`.
pub const MAX_JOB_OUTPUT_NAME_BYTES: usize = 64;

/// Maximum UTF-8 byte length of a structured output value.
///
/// Values are exact machine-readable strings: invalid values are rejected,
/// never sanitized or truncated.
pub const MAX_JOB_OUTPUT_VALUE_BYTES: usize = 512;

/// One validated, immutable structured result from a successful job.
///
/// Names match `[a-z][a-z0-9_]*`; values are one line and contain no control
/// characters or Unicode line separators. Empty values are allowed so a
/// producer can distinguish an intentionally empty result from an absent key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutput {
    name: String,
    value: String,
}

impl JobOutput {
    /// Validate and construct one structured output without truncation.
    pub fn new(
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, JobCompletionError> {
        let name = name.into();
        let value = value.into();
        validate_output_name(&name)?;
        validate_output_value(&value)?;
        Ok(Self { name, value })
    }

    /// Stable machine-readable output name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Exact machine-readable output value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Successful job result: a human detail plus small structured outputs.
///
/// The detail remains unsanitized while work is running and is sanitized
/// exactly once when the terminal snapshot is published. Outputs are
/// validated eagerly and retained exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCompletion {
    detail: String,
    outputs: Vec<JobOutput>,
}

impl JobCompletion {
    /// Start a successful completion with no structured outputs.
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            outputs: Vec::new(),
        }
    }

    /// Add one validated output, rejecting duplicates and count overflow.
    ///
    /// This consuming builder supports direct chaining. Its error converts to
    /// `String`, so `?` also works inside a [`JobManager::spawn_with_completion`]
    /// closure.
    pub fn with_output(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, JobCompletionError> {
        let output = JobOutput::new(name, value)?;
        if self
            .outputs
            .iter()
            .any(|existing| existing.name == output.name)
        {
            return Err(JobCompletionError::DuplicateName);
        }
        if self.outputs.len() >= MAX_JOB_OUTPUTS {
            return Err(JobCompletionError::TooManyOutputs);
        }
        self.outputs.push(output);
        Ok(self)
    }

    /// Human-readable completion detail, sanitized only at publication.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Validated outputs in builder insertion order.
    pub fn outputs(&self) -> &[JobOutput] {
        &self.outputs
    }
}

/// Typed validation failure while constructing a [`JobOutput`] or
/// [`JobCompletion`].
///
/// Variants carry no input text, and every display message is bounded, so
/// hostile names or values are never reflected into logs or job failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobCompletionError {
    /// The output name was empty.
    EmptyName,
    /// The output name exceeded [`MAX_JOB_OUTPUT_NAME_BYTES`].
    NameTooLong,
    /// The output name did not match `[a-z][a-z0-9_]*`.
    InvalidName,
    /// The output value exceeded [`MAX_JOB_OUTPUT_VALUE_BYTES`].
    ValueTooLong,
    /// The output value contained a control or line-separator character.
    InvalidValue,
    /// The completion already contained an output with this name.
    DuplicateName,
    /// The completion already contained [`MAX_JOB_OUTPUTS`] outputs.
    TooManyOutputs,
}

impl fmt::Display for JobCompletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyName => "job output name must not be empty",
            Self::NameTooLong => "job output name exceeds 64 ASCII bytes",
            Self::InvalidName => "job output name must match [a-z][a-z0-9_]*",
            Self::ValueTooLong => "job output value exceeds 512 UTF-8 bytes",
            Self::InvalidValue => "job output value must be one line without control characters",
            Self::DuplicateName => "job completion contains a duplicate output name",
            Self::TooManyOutputs => "job completion exceeds 8 structured outputs",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for JobCompletionError {}

impl From<JobCompletionError> for String {
    fn from(error: JobCompletionError) -> Self {
        error.to_string()
    }
}

/// Registry-unique job handle. Ids come from a per-manager counter and are
/// never reused, so a stale id (a pruned job) reads as unknown rather than
/// aliasing a newer job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(u64);

impl JobId {
    /// Rebuild an id received over an app/tool boundary. Zero and stale ids
    /// are harmless: manager lookups simply report them as unknown.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Stable integer form for Slint models, JSON tool results, and logs.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    /// Done / Failed / Cancelled — the job's thread has exited and its
    /// snapshot will never change again.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Done | JobState::Failed | JobState::Cancelled
        )
    }
}

/// Point-in-time view of one job, cheap to clone into UI models.
#[derive(Debug, Clone)]
pub struct JobSnapshot {
    pub id: JobId,
    /// Human label ("Exporting draft.mp4", "Transcribing interview.mov"),
    /// sanitized to one line and at most [`MAX_JOB_LABEL_BYTES`] UTF-8 bytes.
    pub label: String,
    pub state: JobState,
    /// Whether [`JobManager::cancel`] still accepts cancellation for this job.
    ///
    /// This is `true` while a queued/running job is in cancellable preparation
    /// (including after a request has already been accepted), and becomes
    /// `false` when [`JobContext::try_begin_commit`] wins the ordered boundary.
    /// Every terminal snapshot is non-cancellable.
    pub cancellable: bool,
    /// `0.0..=1.0` when the job reports progress; `None` for indeterminate.
    /// Snaps to `Some(1.0)` on [`JobState::Done`] if the job ever reported;
    /// Failed/Cancelled keep the last reported value.
    pub progress: Option<f32>,
    /// One-line current detail ("frame 1200/3600"), the failure reason after
    /// [`JobState::Failed`], or the result summary after [`JobState::Done`],
    /// sanitized to at most [`MAX_JOB_DETAIL_BYTES`] UTF-8 bytes.
    pub detail: String,
    /// Validated machine-readable results, atomically published only on
    /// [`JobState::Done`] from [`JobManager::spawn_with_completion`].
    ///
    /// Queued, Running, Failed, Cancelled, panicked, and thread-spawn-failed
    /// snapshots always contain an empty vector.
    pub outputs: Vec<JobOutput>,
    /// Cancellation was accepted but the job hasn't exited yet — render as
    /// "cancelling…". Stays set on the terminal snapshot even when a panic
    /// makes that snapshot Failed rather than Cancelled.
    pub cancel_requested: bool,
    /// When [`spawn`](JobManager::spawn) registered the job (threads start
    /// immediately, so this is run start for practical purposes).
    pub started: Instant,
    pub finished: Option<Instant>,
}

/// An owned, read-only view of one job's cooperative cancellation flag.
///
/// Cloning this token only clones an [`Arc`], so it can cheaply move into
/// APIs that retain `Send + Sync + 'static` callbacks. It intentionally
/// exposes no cancellation authority. A token may outlive both the
/// [`JobContext`] and the job: after the job reaches a terminal state it
/// continues to report whether cancellation was ever accepted.
#[derive(Clone)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self { flag }
    }

    /// True once [`JobManager::cancel`] accepted cancellation for this job.
    /// A request rejected after [`JobContext::try_begin_commit`] leaves this
    /// false.
    ///
    /// This check is lock-free and uses [`Ordering::Relaxed`]. The flag is
    /// only a historical cancellation signal; it neither publishes nor
    /// protects other data, while the registry mutex orders job state.
    pub fn cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

/// Handle a running job uses to report progress and to notice cancellation.
/// Deliberately not `Clone` and only lent to the work closure, so progress
/// can never arrive from a job that already returned.
pub struct JobContext {
    id: JobId,
    shared: Arc<Shared>,
    cancellation: CancellationToken,
}

impl JobContext {
    /// Report progress. `fraction` clamps to `0.0..=1.0` (NaN reads as 0);
    /// `detail` replaces the one-line status ("frame 1200/3600").
    pub fn set_progress(&self, fraction: f32, detail: impl Into<String>) {
        let fraction = if fraction.is_nan() {
            0.0
        } else {
            fraction.clamp(0.0, 1.0)
        };
        let detail = sanitize_detail(&detail.into());
        self.shared.update(self.id, |job| {
            job.snap.progress = Some(fraction);
            job.snap.detail = detail;
        });
    }

    /// Return a cheap-clone, owned cancellation check for retained callbacks.
    ///
    /// The token shares this context's flag but cannot request cancellation
    /// or report progress, and keeping it does not keep the context alive.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// True once [`JobManager::cancel`] accepted cancellation for this job.
    /// Lock-free; check it as often as an inner loop likes.
    pub fn cancelled(&self) -> bool {
        self.cancellation.cancelled()
    }

    /// Atomically leave cancellable preparation and begin irreversible commit.
    ///
    /// Call this immediately before the first operation that cannot safely be
    /// rolled back (for example, a SQLite commit, file publication, or export
    /// finalization). It is ordered against [`JobManager::cancel`] by the same
    /// registry mutex, so exactly one side wins:
    ///
    /// - `false`: cancellation was already accepted, or the job is
    ///   unexpectedly missing/terminal. The caller must skip commit.
    /// - `true`: cancellation is closed for this job. Every later cancel
    ///   request is rejected without changing the token or snapshot.
    ///
    /// Repeated calls by the same still-running job are idempotently `true`
    /// after the first successful call and do not emit duplicate snapshots.
    /// The first successful call publishes a snapshot with
    /// [`JobSnapshot::cancellable`] set to `false`.
    pub fn try_begin_commit(&self) -> bool {
        self.shared.try_begin_commit(self.id)
    }
}

/// Clone-able registry handle. One per app; every subsystem spawns through
/// a clone, and all clones share the same registry.
#[derive(Clone)]
pub struct JobManager {
    shared: Arc<Shared>,
}

impl JobManager {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                next_id: AtomicU64::new(1),
                inner: Mutex::default(),
            }),
        }
    }

    /// Run `work` on its own named thread ("job: {label}", truncated). The
    /// closure's `Ok(summary)` / `Err(reason)` becomes the final detail. A
    /// cancellation accepted before the boundary makes a non-panicking return
    /// `Cancelled` regardless of `Ok`/`Err`; after
    /// [`JobContext::try_begin_commit`] succeeds, later cancellation is
    /// rejected and the closure result decides the state.
    pub fn spawn(
        &self,
        label: impl Into<String>,
        work: impl FnOnce(&JobContext) -> Result<String, String> + Send + 'static,
    ) -> JobId {
        self.spawn_with_completion(label, move |context| work(context).map(JobCompletion::new))
    }

    /// Run work that can return bounded structured outputs on success.
    ///
    /// Lifecycle, cancellation, progress, event ordering, pruning, thread
    /// naming, and detail sanitization are identical to [`spawn`](Self::spawn).
    /// Outputs are atomically included only in the terminal Done snapshot. An
    /// accepted cancellation discards a returned completion's outputs; failed,
    /// panicked, and thread-spawn-failed jobs publish none.
    pub fn spawn_with_completion(
        &self,
        label: impl Into<String>,
        work: impl FnOnce(&JobContext) -> Result<JobCompletion, String> + Send + 'static,
    ) -> JobId {
        let label = sanitize_label(&label.into());
        let id = JobId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));

        let (snapshot, subscribers) = {
            let mut inner = self.shared.inner.lock().expect("job registry lock");
            let snap = JobSnapshot {
                id,
                label: label.clone(),
                state: JobState::Queued,
                cancellable: true,
                progress: None,
                detail: String::new(),
                outputs: Vec::new(),
                cancel_requested: false,
                started: Instant::now(),
                finished: None,
            };
            inner.jobs.push(Job {
                snap: snap.clone(),
                cancel: cancel.clone(),
                cancellation_open: true,
            });
            (snap, inner.subscribers.clone())
        };
        // Deliver Queued before the thread exists, so a subscriber's event
        // stream for this job can never open with Running.
        deliver_to_subscribers(&snapshot, &subscribers);

        let context = JobContext {
            id,
            shared: Arc::clone(&self.shared),
            cancellation: CancellationToken::new(cancel),
        };
        let shared = Arc::clone(&self.shared);
        let spawned = thread::Builder::new()
            .name(thread_name(&label))
            .spawn(move || {
                shared.update(id, |job| job.snap.state = JobState::Running);
                let result = catch_unwind(AssertUnwindSafe(|| work(&context)));
                let (base_state, detail, outputs, panicked) = match result {
                    // Panic wins over cancellation: a job that blows up on
                    // the way out is a bug to surface, not a clean stop.
                    Err(payload) => (
                        JobState::Failed,
                        format!("panicked: {}", panic_message(&*payload)),
                        Vec::new(),
                        true,
                    ),
                    Ok(Ok(completion)) => {
                        (JobState::Done, completion.detail, completion.outputs, false)
                    }
                    Ok(Err(reason)) => (JobState::Failed, reason, Vec::new(), false),
                };
                // Normalize caller-sized text before taking the global
                // registry lock. The bounded result is still published
                // atomically with terminal state and structured outputs.
                let detail = sanitize_detail(&detail);
                shared.update(id, |job| {
                    // Decide under the same registry lock as `cancel`: if
                    // cancellation wins the race before this terminal write,
                    // `cancel()` returned true and the terminal state must
                    // agree. If this write wins, cancel sees terminal and
                    // returns false. Panic remains a failure either way.
                    let state = if !panicked && job.cancel.load(Ordering::Relaxed) {
                        JobState::Cancelled
                    } else {
                        base_state
                    };
                    job.snap.state = state;
                    job.snap.detail = detail;
                    if state == JobState::Done {
                        job.snap.outputs = outputs;
                    } else {
                        job.snap.outputs.clear();
                    }
                    job.snap.finished = Some(Instant::now());
                    if state == JobState::Done {
                        // A progress-reporting job that completed is 100%
                        // done; indeterminate jobs stay indeterminate.
                        job.snap.progress = job.snap.progress.map(|_| 1.0);
                    }
                });
            });

        if let Err(err) = spawned {
            // Thread spawn failure (resource exhaustion) is the one way a
            // job dies without running; surface it like any other failure.
            let detail = sanitize_detail(&format!("failed to spawn thread: {err}"));
            self.shared.update(id, |job| {
                job.snap.state = JobState::Failed;
                job.snap.detail = detail;
                job.snap.outputs.clear();
                job.snap.finished = Some(Instant::now());
            });
        }
        id
    }

    /// Cooperative cancel during a job's preparation phase. When accepted,
    /// flips the job's flag; the job exits at its next
    /// [`JobContext::cancelled`] check. Returns `false` for unknown, terminal,
    /// or commit-phase jobs.
    ///
    /// No subscriber notification fires here — the request doesn't change
    /// the job's state, and an out-of-band event from the canceller's thread
    /// could be delivered after the job's own terminal event and wedge a UI
    /// on a stale "cancelling" row. The request is visible immediately via
    /// [`get`](Self::get) / [`snapshot`](Self::snapshot)
    /// (`cancel_requested`), and to subscribers in the eventual terminal
    /// event.
    pub fn cancel(&self, id: JobId) -> bool {
        let mut inner = self.shared.inner.lock().expect("job registry lock");
        let Some(index) = inner.index_of(id) else {
            return false;
        };
        let job = &mut inner.jobs[index];
        if job.snap.state.is_terminal() || !job.cancellation_open {
            return false;
        }
        debug_assert!(job.snap.cancellable);
        job.cancel.store(true, Ordering::Relaxed);
        job.snap.cancel_requested = true;
        true
    }

    pub fn get(&self, id: JobId) -> Option<JobSnapshot> {
        let inner = self.shared.inner.lock().expect("job registry lock");
        inner
            .index_of(id)
            .map(|index| inner.jobs[index].snap.clone())
    }

    /// All known jobs, newest first (running and finished alike; finished
    /// jobs are pruned beyond [`KEEP_FINISHED`]).
    pub fn snapshot(&self) -> Vec<JobSnapshot> {
        let inner = self.shared.inner.lock().expect("job registry lock");
        inner
            .jobs
            .iter()
            .rev()
            .map(|job| job.snap.clone())
            .collect()
    }

    /// Observe every state/progress transition — the UI event-loop bridge.
    /// Multiple subscribers allowed; there is no unsubscribe (subscribers
    /// live as long as the registry, i.e. the app).
    ///
    /// Callbacks run on the job's thread (`Queued` on the spawning thread)
    /// with **no registry lock held**, in per-job lifecycle order. Each
    /// invocation is panic-contained independently; a panicking subscriber
    /// is skipped for that event while later subscribers are still called.
    /// Keep callbacks cheap — post into an event loop rather than doing work
    /// inline. To catch up on pre-existing jobs, subscribe first, then render
    /// one [`snapshot`](Self::snapshot).
    pub fn subscribe(&self, subscriber: impl Fn(JobSnapshot) + Send + Sync + 'static) {
        let mut inner = self.shared.inner.lock().expect("job registry lock");
        inner.subscribers.push(Arc::new(subscriber));
    }
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new()
    }
}

type Subscriber = Arc<dyn Fn(JobSnapshot) + Send + Sync>;

/// Deliver one transition outside the registry lock. Each callback gets its
/// own unwind boundary so subscriber bugs cannot abort lifecycle transitions
/// or suppress delivery to the rest of the subscriber list.
fn deliver_to_subscribers(snapshot: &JobSnapshot, subscribers: &[Subscriber]) {
    for subscriber in subscribers {
        let event = snapshot.clone();
        let _ = catch_unwind(AssertUnwindSafe(|| subscriber(event)));
    }
}

struct Shared {
    /// Ids start at 1 and never recycle (see [`JobId`]).
    next_id: AtomicU64,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Spawn order — strictly ascending id — so `snapshot` reverses it and
    /// lookups binary-search it.
    jobs: Vec<Job>,
    subscribers: Vec<Subscriber>,
}

struct Job {
    snap: JobSnapshot,
    cancel: Arc<AtomicBool>,
    /// Guarded by `Shared::inner`; the ordering point shared by cancellation
    /// requests and `JobContext::try_begin_commit`.
    cancellation_open: bool,
}

impl Shared {
    /// Close cancellation under the same mutex used by `JobManager::cancel`,
    /// then publish the boundary snapshot after dropping the guard.
    fn try_begin_commit(&self, id: JobId) -> bool {
        let (snapshot, subscribers) = {
            let mut inner = self.inner.lock().expect("job registry lock");
            let Some(index) = inner.index_of(id) else {
                // A context should only call this while its job is registered,
                // but missing/terminal must fail closed: never publish without
                // winning the ordered boundary.
                return false;
            };
            let job = &mut inner.jobs[index];
            if job.snap.state.is_terminal() {
                return false;
            }

            let flag = job.cancel.load(Ordering::Relaxed);
            debug_assert_eq!(flag, job.snap.cancel_requested);
            if flag || job.snap.cancel_requested {
                return false;
            }

            debug_assert_eq!(job.cancellation_open, job.snap.cancellable);
            if !job.cancellation_open {
                // The same live context already won. Preserve idempotence
                // without producing a second boundary notification.
                return true;
            }

            job.cancellation_open = false;
            job.snap.cancellable = false;
            (job.snap.clone(), inner.subscribers.clone())
        };

        deliver_to_subscribers(&snapshot, &subscribers);
        true
    }

    /// Mutate one job under the lock, then deliver the resulting snapshot to
    /// every subscriber **after** the guard is dropped (callbacks may
    /// re-enter the registry freely). Terminal transitions prune — the job
    /// that just finished is by definition the newest-finished, so it always
    /// survives its own prune.
    fn update(&self, id: JobId, mutate: impl FnOnce(&mut Job)) {
        let (snapshot, subscribers) = {
            let mut inner = self.inner.lock().expect("job registry lock");
            let Some(index) = inner.index_of(id) else {
                return;
            };
            let job = &mut inner.jobs[index];
            mutate(job);
            if job.snap.state.is_terminal() {
                job.cancellation_open = false;
                job.snap.cancellable = false;
            }
            debug_assert_eq!(job.cancellation_open, job.snap.cancellable);
            let snapshot = job.snap.clone();
            if snapshot.state.is_terminal() {
                inner.prune_finished();
            }
            (snapshot, inner.subscribers.clone())
        };
        deliver_to_subscribers(&snapshot, &subscribers);
    }
}

impl Inner {
    fn index_of(&self, id: JobId) -> Option<usize> {
        self.jobs.binary_search_by_key(&id, |job| job.snap.id).ok()
    }

    fn prune_finished(&mut self) {
        let finished = self
            .jobs
            .iter()
            .filter(|j| j.snap.state.is_terminal())
            .count();
        let excess = finished.saturating_sub(KEEP_FINISHED);
        if excess == 0 {
            return;
        }
        // Prune by *finish* recency, not spawn order: a long-running job
        // that just ended is the freshest news on the board even if it was
        // spawned before everything else. `None` (impossible for terminal
        // jobs) would sort oldest, which is the safe direction anyway.
        let mut doomed: Vec<(Option<Instant>, JobId)> = self
            .jobs
            .iter()
            .filter(|j| j.snap.state.is_terminal())
            .map(|j| (j.snap.finished, j.snap.id))
            .collect();
        doomed.sort_unstable();
        doomed.truncate(excess);
        self.jobs
            .retain(|job| !doomed.iter().any(|(_, id)| *id == job.snap.id));
    }
}

const MAX_THREAD_NAME_BYTES: usize = 32;
const ELLIPSIS: char = '…';

fn validate_output_name(name: &str) -> Result<(), JobCompletionError> {
    if name.is_empty() {
        return Err(JobCompletionError::EmptyName);
    }
    if name.len() > MAX_JOB_OUTPUT_NAME_BYTES {
        return Err(JobCompletionError::NameTooLong);
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase()
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        return Err(JobCompletionError::InvalidName);
    }
    Ok(())
}

fn validate_output_value(value: &str) -> Result<(), JobCompletionError> {
    if value.len() > MAX_JOB_OUTPUT_VALUE_BYTES {
        return Err(JobCompletionError::ValueTooLong);
    }
    if value
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        return Err(JobCompletionError::InvalidValue);
    }
    Ok(())
}

fn sanitize_label(label: &str) -> String {
    sanitize_text(label, MAX_JOB_LABEL_BYTES)
}

fn sanitize_detail(detail: &str) -> String {
    sanitize_text(detail, MAX_JOB_DETAIL_BYTES)
}

/// Replace controls and Unicode line separators, then retain at most
/// `max_bytes` of valid UTF-8. Truncated values have exactly one appended
/// ellipsis character within the cap. Applying this to its own output is a
/// no-op.
fn sanitize_text(value: &str, max_bytes: usize) -> String {
    debug_assert!(max_bytes >= ELLIPSIS.len_utf8());

    let mut sanitized = String::with_capacity(value.len().min(max_bytes));
    let mut truncated = false;
    for character in value.chars() {
        let safe = if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            ' '
        } else {
            character
        };
        if sanitized.len() + safe.len_utf8() > max_bytes {
            truncated = true;
            break;
        }
        sanitized.push(safe);
    }

    if truncated {
        while sanitized.len() + ELLIPSIS.len_utf8() > max_bytes {
            sanitized.pop();
        }
        sanitized.push(ELLIPSIS);
    }
    sanitized
}

/// Worker threads are named so profilers, debuggers, and crash reports
/// attribute the work. Capped at 32 bytes on a char boundary — platforms
/// impose their own hard limits (15 bytes on Linux), we just keep names tidy
/// and valid UTF-8 ourselves.
fn thread_name(label: &str) -> String {
    let mut name = format!("job: {label}");
    if name.len() > MAX_THREAD_NAME_BYTES {
        let mut end = MAX_THREAD_NAME_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    name
}

/// Best-effort extraction of a panic payload: `panic!` with a literal
/// carries `&str`, with formatting `String`; anything else is opaque.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "opaque panic payload"
    }
}

#[cfg(test)]
mod tests;
