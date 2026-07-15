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
//!   finished jobs beyond the newest [`KEEP_FINISHED`] are pruned (by
//!   *finish* time — a long export that just ended is recent news even if it
//!   was spawned first). Running jobs are never pruned.
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
                let (base_state, detail, panicked) = match result {
                    // Panic wins over cancellation: a job that blows up on
                    // the way out is a bug to surface, not a clean stop.
                    Err(payload) => (
                        JobState::Failed,
                        sanitize_detail(&format!("panicked: {}", panic_message(&*payload))),
                        true,
                    ),
                    Ok(Ok(summary)) => (JobState::Done, sanitize_detail(&summary), false),
                    Ok(Err(reason)) => (JobState::Failed, sanitize_detail(&reason), false),
                };
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
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// A manager plus a channel carrying every subscriber event, so tests
    /// rendezvous on real transitions instead of sleeping.
    fn manager_with_events() -> (JobManager, mpsc::Receiver<JobSnapshot>) {
        let manager = JobManager::new();
        let (tx, rx) = mpsc::channel();
        manager.subscribe(move |snapshot| {
            let _ = tx.send(snapshot);
        });
        (manager, rx)
    }

    /// Drain events for `id` (skipping other jobs') until its terminal
    /// event; returns the job's full ordered event history.
    fn wait_history(rx: &mpsc::Receiver<JobSnapshot>, id: JobId) -> Vec<JobSnapshot> {
        let mut events = Vec::new();
        loop {
            let snapshot = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("timed out waiting for job events");
            if snapshot.id != id {
                continue;
            }
            let terminal = snapshot.state.is_terminal();
            if terminal {
                assert!(
                    !snapshot.cancellable,
                    "terminal job {id} still accepts cancellation"
                );
            }
            events.push(snapshot);
            if terminal {
                return events;
            }
        }
    }

    fn assert_one_line_bounded(value: &str, max_bytes: usize) {
        assert!(
            value.len() <= max_bytes,
            "{} bytes exceeds {max_bytes}",
            value.len()
        );
        assert!(std::str::from_utf8(value.as_bytes()).is_ok());
        assert!(
            !value
                .chars()
                .any(|character| character.is_control()
                    || matches!(character, '\u{2028}' | '\u{2029}')),
            "unsafe one-line value: {value:?}"
        );
    }

    fn oversized_text(prefix: &str, cap: usize) -> String {
        let mut value = format!("{prefix}\n\r\t\0\u{2028}\u{2029}");
        while value.len() <= cap + 8 {
            value.push_str("动画🦀");
        }
        value
    }

    #[test]
    fn cancellation_token_has_callback_safe_traits() {
        fn assert_traits<T: Clone + Send + Sync + 'static>() {}

        assert_traits::<CancellationToken>();
    }

    #[test]
    fn cancellation_token_moves_into_static_callback_and_observes_cancel() {
        type CancelCallback = Arc<dyn Fn() -> bool + Send + Sync + 'static>;

        let (manager, rx) = manager_with_events();
        let (callback_tx, callback_rx) = mpsc::channel::<CancelCallback>();
        let (resume_tx, resume_rx) = mpsc::channel();
        let id = manager.spawn("callback cancellation", move |ctx| {
            let token = ctx.cancellation_token();
            let callback: CancelCallback = Arc::new(move || token.cancelled());
            if callback_tx.send(Arc::clone(&callback)).is_err() {
                return Err("callback receiver dropped".into());
            }
            resume_rx
                .recv()
                .map_err(|_| "resume sender dropped".to_owned())?;
            assert!(callback());
            Ok("callback observed cancellation".into())
        });

        let callback = callback_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("job did not publish its cancellation callback");
        assert!(!callback());
        assert!(manager.cancel(id));
        assert!(callback());
        resume_tx.send(()).unwrap();

        let terminal = wait_history(&rx, id).pop().unwrap();
        assert_eq!(terminal.state, JobState::Cancelled);
    }

    #[test]
    fn cancellation_token_and_context_agree_before_cancellation() {
        let (manager, rx) = manager_with_events();
        let (state_tx, state_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let id = manager.spawn("matching cancellation state", move |ctx| {
            let token = ctx.cancellation_token();
            state_tx
                .send((ctx.cancelled(), token.cancelled()))
                .map_err(|_| "state receiver dropped".to_owned())?;
            resume_rx
                .recv()
                .map_err(|_| "resume sender dropped".to_owned())?;
            Ok("states matched".into())
        });

        let (context_cancelled, token_cancelled) = state_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("job did not publish cancellation state");
        assert_eq!(context_cancelled, token_cancelled);
        assert!(!context_cancelled);
        resume_tx.send(()).unwrap();
        assert_eq!(wait_history(&rx, id).last().unwrap().state, JobState::Done);
    }

    #[test]
    fn cancellation_tokens_remain_read_only_after_terminal_completion() {
        let (manager, rx) = manager_with_events();

        let (uncancelled_tx, uncancelled_rx) = mpsc::channel();
        let uncancelled_id = manager.spawn("uncancelled token", move |ctx| {
            uncancelled_tx
                .send(ctx.cancellation_token())
                .map_err(|_| "token receiver dropped".to_owned())?;
            Ok("finished normally".into())
        });
        let uncancelled = uncancelled_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("uncancelled job did not publish its token");
        assert_eq!(
            wait_history(&rx, uncancelled_id).last().unwrap().state,
            JobState::Done
        );
        assert!(!uncancelled.cancelled());

        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let cancelled_id = manager.spawn("cancelled token", move |ctx| {
            cancelled_tx
                .send(ctx.cancellation_token())
                .map_err(|_| "token receiver dropped".to_owned())?;
            resume_rx
                .recv()
                .map_err(|_| "resume sender dropped".to_owned())?;
            Ok("stopped".into())
        });
        let cancelled = cancelled_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("cancelled job did not publish its token");
        assert!(!cancelled.cancelled());
        assert!(manager.cancel(cancelled_id));
        resume_tx.send(()).unwrap();
        assert_eq!(
            wait_history(&rx, cancelled_id).last().unwrap().state,
            JobState::Cancelled
        );
        assert!(cancelled.cancelled());

        // Neither token keeps the context or registry alive, and both retain
        // their final read-only view after those owners are gone.
        drop(manager);
        assert!(!uncancelled.cancelled());
        assert!(cancelled.cancelled());
    }

    #[test]
    fn cancellation_wins_before_commit_boundary_and_prevents_commit() {
        let (manager, rx) = manager_with_events();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (boundary_tx, boundary_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let committed = Arc::new(AtomicBool::new(false));
        let committed_in_job = Arc::clone(&committed);
        let id = manager.spawn("cancel before commit", move |ctx| {
            let token = ctx.cancellation_token();
            ready_tx.send(()).unwrap();
            resume_rx.recv().unwrap();

            let may_commit = ctx.try_begin_commit();
            boundary_tx
                .send((may_commit, ctx.cancelled(), token.cancelled()))
                .unwrap();
            if may_commit {
                committed_in_job.store(true, Ordering::Relaxed);
            }
            finish_rx.recv().unwrap();
            Ok("preparation stopped".into())
        });

        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("job did not reach cancellable preparation");
        let running = manager.get(id).unwrap();
        assert_eq!(running.state, JobState::Running);
        assert!(running.cancellable);
        assert!(!running.cancel_requested);

        assert!(manager.cancel(id));
        let requested = manager.get(id).unwrap();
        assert!(requested.cancellable);
        assert!(requested.cancel_requested);
        resume_tx.send(()).unwrap();

        assert_eq!(
            boundary_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("job did not attempt commit"),
            (false, true, true)
        );
        let still_cancelled = manager.get(id).unwrap();
        assert_eq!(still_cancelled.state, JobState::Running);
        assert!(still_cancelled.cancellable);
        assert!(still_cancelled.cancel_requested);
        assert!(!committed.load(Ordering::Relaxed));
        finish_tx.send(()).unwrap();

        let history = wait_history(&rx, id);
        let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
        assert_eq!(
            states,
            [JobState::Queued, JobState::Running, JobState::Cancelled]
        );
        let terminal = history.last().unwrap();
        assert!(terminal.cancel_requested);
    }

    #[test]
    fn commit_boundary_wins_before_cancel_and_preserves_false_token() {
        let (manager, rx) = manager_with_events();
        let (boundary_tx, boundary_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (observed_tx, observed_rx) = mpsc::channel();
        let committed = Arc::new(AtomicBool::new(false));
        let committed_in_job = Arc::clone(&committed);
        let id = manager.spawn("commit before cancel", move |ctx| {
            let token = ctx.cancellation_token();
            assert!(ctx.try_begin_commit());
            boundary_tx.send(token.clone()).unwrap();
            resume_rx.recv().unwrap();
            observed_tx
                .send((ctx.cancelled(), token.cancelled()))
                .unwrap();
            committed_in_job.store(true, Ordering::Relaxed);
            Ok("published".into())
        });

        let token = boundary_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("job did not cross commit boundary");
        let committing = manager.get(id).unwrap();
        assert_eq!(committing.state, JobState::Running);
        assert!(!committing.cancellable);
        assert!(!committing.cancel_requested);
        assert!(!token.cancelled());

        assert!(!manager.cancel(id));
        assert!(!manager.cancel(id));
        assert!(!token.cancelled());
        let after_rejection = manager.get(id).unwrap();
        assert!(!after_rejection.cancellable);
        assert!(!after_rejection.cancel_requested);

        resume_tx.send(()).unwrap();
        assert_eq!(
            observed_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("job did not report post-boundary cancellation state"),
            (false, false)
        );
        let terminal = wait_history(&rx, id).pop().unwrap();
        assert_eq!(terminal.state, JobState::Done);
        assert!(!terminal.cancel_requested);
        assert!(committed.load(Ordering::Relaxed));
        assert!(!token.cancelled());
    }

    #[test]
    fn cancel_and_commit_boundary_race_has_exactly_one_winner() {
        const ROUNDS: usize = 128;

        let (manager, rx) = manager_with_events();
        for round in 0..ROUNDS {
            let start = Arc::new(std::sync::Barrier::new(3));
            let worker_start = Arc::clone(&start);
            let (ready_tx, ready_rx) = mpsc::channel();
            let (boundary_tx, boundary_rx) = mpsc::channel();
            let id = manager.spawn(format!("commit race {round}"), move |ctx| {
                ready_tx.send(()).unwrap();
                worker_start.wait();
                let boundary_won = ctx.try_begin_commit();
                boundary_tx.send(boundary_won).unwrap();
                Ok(if boundary_won {
                    "committed".into()
                } else {
                    "cancelled".into()
                })
            });

            ready_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("worker did not reach race gate");
            let cancel_manager = manager.clone();
            let canceller_start = Arc::clone(&start);
            let canceller = thread::spawn(move || {
                canceller_start.wait();
                cancel_manager.cancel(id)
            });

            start.wait();
            let boundary_won = boundary_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("worker did not report race result");
            let cancel_won = canceller.join().expect("canceller panicked");
            assert_eq!(
                boundary_won, !cancel_won,
                "round {round}: cancel={cancel_won}, boundary={boundary_won}"
            );

            let terminal = wait_history(&rx, id).pop().unwrap();
            assert_eq!(
                terminal.state,
                if boundary_won {
                    JobState::Done
                } else {
                    JobState::Cancelled
                },
                "round {round}"
            );
            assert_eq!(terminal.cancel_requested, cancel_won, "round {round}");
        }
    }

    #[test]
    fn failure_and_panic_after_commit_boundary_remain_failed() {
        let (manager, rx) = manager_with_events();

        let (failure_ready_tx, failure_ready_rx) = mpsc::channel();
        let (fail_tx, fail_rx) = mpsc::channel();
        let failure_id = manager.spawn("post-commit failure", move |ctx| {
            assert!(ctx.try_begin_commit());
            failure_ready_tx.send(ctx.cancellation_token()).unwrap();
            fail_rx.recv().unwrap();
            Err("publication failed".into())
        });
        let failure_token = failure_ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("failure job did not cross commit boundary");
        assert!(!manager.cancel(failure_id));
        assert!(!failure_token.cancelled());
        fail_tx.send(()).unwrap();
        let failed = wait_history(&rx, failure_id).pop().unwrap();
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.detail, "publication failed");
        assert!(!failed.cancel_requested);
        assert!(!failure_token.cancelled());

        let (panic_ready_tx, panic_ready_rx) = mpsc::channel();
        let (panic_tx, panic_rx) = mpsc::channel();
        let panic_id = manager.spawn("post-commit panic", move |ctx| {
            assert!(ctx.try_begin_commit());
            panic_ready_tx.send(ctx.cancellation_token()).unwrap();
            panic_rx.recv().unwrap();
            panic!("post-commit panic");
        });
        let panic_token = panic_ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("panic job did not cross commit boundary");
        assert!(!manager.cancel(panic_id));
        assert!(!panic_token.cancelled());
        panic_tx.send(()).unwrap();
        let panicked = wait_history(&rx, panic_id).pop().unwrap();
        assert_eq!(panicked.state, JobState::Failed);
        assert_eq!(panicked.detail, "panicked: post-commit panic");
        assert!(!panicked.cancel_requested);
        assert!(!panic_token.cancelled());
    }

    #[test]
    fn commit_boundary_is_idempotent_and_emits_one_exact_transition() {
        let (manager, rx) = manager_with_events();
        let (calls_tx, calls_rx) = mpsc::channel();
        let id = manager.spawn("idempotent commit", move |ctx| {
            let first = ctx.try_begin_commit();
            let second = ctx.try_begin_commit();
            calls_tx.send((first, second)).unwrap();
            Ok("published".into())
        });

        assert_eq!(
            calls_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("job did not report boundary calls"),
            (true, true)
        );
        let history = wait_history(&rx, id);
        let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
        assert_eq!(
            states,
            [
                JobState::Queued,
                JobState::Running,
                JobState::Running,
                JobState::Done,
            ]
        );
        let cancellable: Vec<bool> = history.iter().map(|event| event.cancellable).collect();
        assert_eq!(cancellable, [true, true, false, false]);
        assert!(history.iter().all(|snapshot| !snapshot.cancel_requested));
        assert_eq!(
            history
                .iter()
                .filter(|snapshot| snapshot.state == JobState::Running && !snapshot.cancellable)
                .count(),
            1
        );
    }

    #[test]
    fn commit_boundary_subscribers_are_reentrant_and_panic_isolated() {
        let manager = JobManager::new();
        manager.subscribe(|snapshot| {
            if snapshot.label == "boundary subscribers"
                && snapshot.state == JobState::Running
                && !snapshot.cancellable
            {
                panic!("boundary subscriber failed");
            }
        });

        let reentrant_manager = manager.clone();
        let (reentrant_tx, reentrant_rx) = mpsc::channel();
        manager.subscribe(move |snapshot| {
            if snapshot.label == "boundary subscribers"
                && snapshot.state == JobState::Running
                && !snapshot.cancellable
            {
                let get_saw_closed = reentrant_manager
                    .get(snapshot.id)
                    .is_some_and(|stored| !stored.cancellable);
                let list_saw_closed = reentrant_manager
                    .snapshot()
                    .iter()
                    .any(|stored| stored.id == snapshot.id && !stored.cancellable);
                let cancel_was_rejected = !reentrant_manager.cancel(snapshot.id);
                let _ = reentrant_tx.send((get_saw_closed, list_saw_closed, cancel_was_rejected));
            }
        });

        let (healthy_tx, healthy_rx) = mpsc::channel();
        manager.subscribe(move |snapshot| {
            let _ = healthy_tx.send(snapshot);
        });

        let id = manager.spawn("boundary subscribers", |ctx| {
            assert!(ctx.try_begin_commit());
            Ok("published".into())
        });
        assert_eq!(
            reentrant_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("boundary subscriber deadlocked or was suppressed"),
            (true, true, true)
        );

        let history = wait_history(&healthy_rx, id);
        let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
        assert_eq!(
            states,
            [
                JobState::Queued,
                JobState::Running,
                JobState::Running,
                JobState::Done,
            ]
        );
        assert_eq!(manager.get(id).unwrap().state, JobState::Done);
    }

    #[test]
    fn success_path_events_in_order() {
        let (manager, rx) = manager_with_events();
        let id = manager.spawn("Exporting draft.mp4", |ctx| {
            ctx.set_progress(0.25, "frame 900/3600");
            ctx.set_progress(0.75, "frame 2700/3600");
            Ok("wrote draft.mp4".into())
        });

        let events = wait_history(&rx, id);
        let states: Vec<JobState> = events.iter().map(|e| e.state).collect();
        assert_eq!(
            states,
            [
                JobState::Queued,
                JobState::Running,
                JobState::Running,
                JobState::Running,
                JobState::Done,
            ]
        );
        let progress: Vec<Option<f32>> = events.iter().map(|e| e.progress).collect();
        assert_eq!(progress, [None, None, Some(0.25), Some(0.75), Some(1.0)]);
        assert_eq!(events[2].detail, "frame 900/3600");
        assert_eq!(events.iter().filter(|e| e.state.is_terminal()).count(), 1);

        let done = events.last().unwrap();
        assert_eq!(done.detail, "wrote draft.mp4");
        assert!(done.finished.is_some());

        let got = manager.get(id).expect("finished job still known");
        assert_eq!(got.state, JobState::Done);
        assert_eq!(got.detail, "wrote draft.mp4");
        assert_eq!(got.progress, Some(1.0));
    }

    #[test]
    fn failure_keeps_reason_and_last_progress() {
        let (manager, rx) = manager_with_events();
        let id = manager.spawn("Analyzing clip.mov", |ctx| {
            ctx.set_progress(0.4, "scanning");
            Err("disk full".into())
        });

        let failed = wait_history(&rx, id).pop().unwrap();
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.detail, "disk full");
        // No snap-to-1.0 on failure — the bar stays where the job died.
        assert_eq!(failed.progress, Some(0.4));
        assert!(failed.finished.is_some());
    }

    #[test]
    fn cancel_flag_decides_over_ok_return() {
        let (manager, rx) = manager_with_events();
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let id = manager.spawn("Transcribing interview.mov", move |ctx| {
            started_tx.send(()).unwrap();
            // Deterministic rendezvous: the test cancels while we wait here.
            resume_rx.recv().unwrap();
            assert!(ctx.cancelled());
            Ok("stopped at 00:42".into())
        });

        started_rx.recv().unwrap();
        assert_eq!(manager.get(id).unwrap().state, JobState::Running);
        // Cancel through a clone: all clones share one registry.
        assert!(manager.clone().cancel(id));
        assert!(manager.get(id).unwrap().cancel_requested);
        resume_tx.send(()).unwrap();

        let last = wait_history(&rx, id).pop().unwrap();
        assert_eq!(last.state, JobState::Cancelled);
        assert_eq!(last.detail, "stopped at 00:42");
        assert!(last.cancel_requested);

        // Finished and unknown jobs both refuse.
        assert!(!manager.cancel(id));
        assert!(!manager.cancel(JobId(9999)));
    }

    #[test]
    fn cancel_flag_decides_over_err_return() {
        let (manager, rx) = manager_with_events();
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let id = manager.spawn("Exporting", move |_| {
            started_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            Err("aborted mid-write".into())
        });

        started_rx.recv().unwrap();
        assert!(manager.cancel(id));
        resume_tx.send(()).unwrap();

        let last = wait_history(&rx, id).pop().unwrap();
        assert_eq!(last.state, JobState::Cancelled);
        assert_eq!(last.detail, "aborted mid-write");
    }

    #[test]
    fn panic_becomes_failed() {
        let (manager, rx) = manager_with_events();

        // Literal panic → &str payload.
        let id = manager.spawn("Rendering", |_| panic!("codec exploded"));
        let failed = wait_history(&rx, id).pop().unwrap();
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.detail, "panicked: codec exploded");
        assert!(failed.finished.is_some());

        // Formatted panic → String payload.
        let id = manager.spawn("Rendering", |_| panic!("frame {} bad", 7));
        let failed = wait_history(&rx, id).pop().unwrap();
        assert_eq!(failed.detail, "panicked: frame 7 bad");
    }

    #[test]
    fn progress_clamps_and_is_visible_mid_flight() {
        let (manager, rx) = manager_with_events();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let id = manager.spawn("Clamping", move |ctx| {
            ctx.set_progress(1.5, "over");
            ready_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            ctx.set_progress(-0.5, "under");
            ctx.set_progress(f32::NAN, "nan");
            // Err so the terminal state keeps the last progress unsnapped.
            Err("checked".into())
        });

        ready_rx.recv().unwrap();
        let mid = manager.get(id).unwrap();
        assert_eq!(mid.state, JobState::Running);
        assert_eq!(mid.progress, Some(1.0));
        assert_eq!(mid.detail, "over");
        resume_tx.send(()).unwrap();

        let events = wait_history(&rx, id);
        let progress: Vec<Option<f32>> = events.iter().map(|e| e.progress).collect();
        assert_eq!(
            progress,
            [None, None, Some(1.0), Some(0.0), Some(0.0), Some(0.0)]
        );
    }

    #[test]
    fn snapshot_orders_newest_first() {
        let (manager, rx) = manager_with_events();
        let (gate_a_tx, gate_a_rx) = mpsc::channel::<()>();
        let a = manager.spawn("a", move |_| {
            let _ = gate_a_rx.recv();
            Ok("a done".into())
        });
        let (gate_b_tx, gate_b_rx) = mpsc::channel::<()>();
        let b = manager.spawn("b", move |_| {
            let _ = gate_b_rx.recv();
            Ok("b done".into())
        });
        let c = manager.spawn("c", |_| Ok("c done".into()));
        wait_history(&rx, c);

        // Newest spawned first, regardless of running/finished state.
        let ids: Vec<JobId> = manager.snapshot().iter().map(|s| s.id).collect();
        assert_eq!(ids, [c, b, a]);

        // Release sequentially so each terminal event is awaited in turn.
        drop(gate_a_tx);
        wait_history(&rx, a);
        drop(gate_b_tx);
        wait_history(&rx, b);
    }

    #[test]
    fn prune_keeps_newest_finished_never_running() {
        let (manager, rx) = manager_with_events();

        // Oldest job of all, still running while everything else finishes.
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let runner = manager.spawn("long export", move |_| {
            let _ = gate_rx.recv();
            Ok("finally".into())
        });

        // Finish KEEP_FINISHED + 6 quick jobs strictly one after another, so
        // finish order matches spawn order and the prune is deterministic.
        let mut quick = Vec::new();
        for n in 0..KEEP_FINISHED + 6 {
            let id = manager.spawn(format!("quick {n}"), |_| Ok("done".into()));
            wait_history(&rx, id);
            quick.push(id);
        }

        // The six oldest-finished are gone; the running job survives even
        // though it is the oldest job in the registry.
        assert_eq!(manager.snapshot().len(), KEEP_FINISHED + 1);
        for &id in &quick[..6] {
            assert!(manager.get(id).is_none(), "expected {id} pruned");
        }
        for &id in &quick[6..] {
            assert!(manager.get(id).is_some());
        }
        assert_eq!(manager.get(runner).unwrap().state, JobState::Running);

        // The runner finishes last → newest-finished → survives the prune
        // its own completion triggers; the next-oldest quick job goes. Under
        // spawn-order pruning the runner would have been dropped instead.
        drop(gate_tx);
        wait_history(&rx, runner);
        assert_eq!(manager.get(runner).unwrap().state, JobState::Done);
        assert!(manager.get(quick[6]).is_none());
        assert_eq!(manager.snapshot().len(), KEEP_FINISHED);
    }

    #[test]
    fn multiple_subscribers_all_notified() {
        let manager = JobManager::new();
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        manager.subscribe(move |s| {
            let _ = tx1.send(s.state);
        });
        manager.subscribe(move |s| {
            let _ = tx2.send(s.state);
        });

        manager.spawn("observed", |_| Ok("ok".into()));
        for rx in [rx1, rx2] {
            loop {
                let state = rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("subscriber missed events");
                if state == JobState::Done {
                    break;
                }
            }
        }
    }

    #[test]
    fn queued_subscriber_panic_does_not_block_spawn_or_lifecycle() {
        let manager = JobManager::new();
        manager.subscribe(|snapshot| {
            if snapshot.state == JobState::Queued {
                panic!("queued subscriber failed");
            }
        });
        let (healthy_tx, healthy_rx) = mpsc::channel();
        manager.subscribe(move |snapshot| {
            let _ = healthy_tx.send(snapshot);
        });

        // Queued delivery is synchronous, so reaching this assignment proves
        // the first subscriber's panic did not unwind `spawn`.
        let id = manager.spawn("observed", |ctx| {
            ctx.set_progress(0.5, "halfway");
            Ok("complete".into())
        });
        let history = wait_history(&healthy_rx, id);
        let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
        assert_eq!(
            states,
            [
                JobState::Queued,
                JobState::Running,
                JobState::Running,
                JobState::Done,
            ]
        );
        assert_eq!(manager.get(id).unwrap().state, JobState::Done);
    }

    #[test]
    fn panics_on_worker_notifications_do_not_strand_job() {
        let manager = JobManager::new();
        let running_panics = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let terminal_panics = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let running_panics_in_callback = Arc::clone(&running_panics);
        let terminal_panics_in_callback = Arc::clone(&terminal_panics);
        manager.subscribe(move |snapshot| {
            if snapshot.state == JobState::Running {
                running_panics_in_callback.fetch_add(1, Ordering::Relaxed);
                panic!("running/progress subscriber failed");
            }
            if snapshot.state.is_terminal() {
                terminal_panics_in_callback.fetch_add(1, Ordering::Relaxed);
                panic!("terminal subscriber failed");
            }
        });
        let (healthy_tx, healthy_rx) = mpsc::channel();
        manager.subscribe(move |snapshot| {
            let _ = healthy_tx.send(snapshot);
        });

        let id = manager.spawn("observed", |ctx| {
            ctx.set_progress(0.5, "halfway");
            Ok("complete".into())
        });
        let history = wait_history(&healthy_rx, id);
        let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
        assert_eq!(
            states,
            [
                JobState::Queued,
                JobState::Running,
                JobState::Running,
                JobState::Done,
            ]
        );
        assert_eq!(
            history
                .iter()
                .filter(|event| event.state.is_terminal())
                .count(),
            1
        );
        assert!(healthy_rx.try_recv().is_err());
        assert_eq!(running_panics.load(Ordering::Relaxed), 2);
        assert_eq!(terminal_panics.load(Ordering::Relaxed), 1);
        assert_eq!(manager.get(id).unwrap().state, JobState::Done);
    }

    #[test]
    fn subscriber_can_query_cancel_and_spawn_reentrantly() {
        let manager = JobManager::new();
        let (event_tx, event_rx) = mpsc::channel();
        manager.subscribe(move |snapshot| {
            let _ = event_tx.send(snapshot);
        });

        let reentrant_manager = manager.clone();
        let (reentrant_tx, reentrant_rx) = mpsc::channel();
        manager.subscribe(move |snapshot| {
            if snapshot.label == "outer" && snapshot.state == JobState::Queued {
                let get_saw_queued = reentrant_manager
                    .get(snapshot.id)
                    .is_some_and(|stored| stored.state == JobState::Queued);
                let snapshot_saw_job = reentrant_manager
                    .snapshot()
                    .iter()
                    .any(|stored| stored.id == snapshot.id);
                let cancelled = reentrant_manager.cancel(snapshot.id);
                let inner = reentrant_manager.spawn("inner", |_| Ok("inner done".into()));
                let _ = reentrant_tx.send((get_saw_queued, snapshot_saw_job, cancelled, inner));
            }
        });

        let outer = manager.spawn("outer", |ctx| {
            assert!(ctx.cancelled());
            Ok("outer stopped".into())
        });
        let (get_saw_queued, snapshot_saw_job, cancelled, inner) = reentrant_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("reentrant subscriber did not finish");
        assert!(get_saw_queued);
        assert!(snapshot_saw_job);
        assert!(cancelled);

        let mut terminals = Vec::new();
        while terminals.len() < 2 {
            let snapshot = event_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("timed out waiting for reentrant jobs");
            if snapshot.state.is_terminal() {
                terminals.push((snapshot.id, snapshot.state));
            }
        }
        assert!(terminals.contains(&(inner, JobState::Done)));
        assert!(terminals.contains(&(outer, JobState::Cancelled)));
    }

    #[test]
    fn retained_text_is_safe_bounded_and_deterministic() {
        assert_eq!(
            sanitize_text("a\nb\rc\td\0e\u{2028}f\u{2029}g", MAX_JOB_DETAIL_BYTES),
            "a b c d e f g"
        );

        let label = oversized_text("label:", MAX_JOB_LABEL_BYTES);
        let progress = oversized_text("progress:", MAX_JOB_DETAIL_BYTES);
        let summary = oversized_text("summary:", MAX_JOB_DETAIL_BYTES);
        let expected_label = sanitize_label(&label);
        let expected_progress = sanitize_detail(&progress);
        let expected_summary = sanitize_detail(&summary);
        for (value, cap) in [
            (&expected_label, MAX_JOB_LABEL_BYTES),
            (&expected_progress, MAX_JOB_DETAIL_BYTES),
            (&expected_summary, MAX_JOB_DETAIL_BYTES),
        ] {
            assert_one_line_bounded(value, cap);
            assert!(value.ends_with(ELLIPSIS));
        }
        assert_eq!(sanitize_label(&label), expected_label);
        assert_eq!(sanitize_label(&expected_label), expected_label);
        assert_eq!(sanitize_detail(&progress), expected_progress);
        assert_eq!(sanitize_detail(&expected_progress), expected_progress);
        assert_eq!(sanitize_detail(&summary), expected_summary);
        assert_eq!(sanitize_detail(&expected_summary), expected_summary);

        let (manager, rx) = manager_with_events();
        let (name_tx, name_rx) = mpsc::channel();
        let id = manager.spawn(label, move |ctx| {
            name_tx
                .send(thread::current().name().unwrap_or("").to_owned())
                .unwrap();
            ctx.set_progress(0.5, progress);
            Ok(summary)
        });
        let actual_thread_name = name_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("job did not report its thread name");
        let history = wait_history(&rx, id);
        let states: Vec<JobState> = history.iter().map(|event| event.state).collect();
        assert_eq!(
            states,
            [
                JobState::Queued,
                JobState::Running,
                JobState::Running,
                JobState::Done,
            ]
        );
        assert_eq!(actual_thread_name, thread_name(&expected_label));
        assert!(actual_thread_name.len() <= MAX_THREAD_NAME_BYTES);
        for snapshot in &history {
            assert_eq!(snapshot.label, expected_label);
            assert_one_line_bounded(&snapshot.label, MAX_JOB_LABEL_BYTES);
            assert_one_line_bounded(&snapshot.detail, MAX_JOB_DETAIL_BYTES);
        }
        assert_eq!(history[2].detail, expected_progress);
        assert_eq!(history[3].detail, expected_summary);

        let error = oversized_text("error:", MAX_JOB_DETAIL_BYTES);
        let expected_error = sanitize_detail(&error);
        let error_id = manager.spawn("error job", move |_| Err(error));
        let error_history = wait_history(&rx, error_id);
        let failed = error_history.last().unwrap();
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.detail, expected_error);
        assert_one_line_bounded(&failed.detail, MAX_JOB_DETAIL_BYTES);
        assert_eq!(sanitize_detail(&failed.detail), failed.detail);

        let panic_payload = oversized_text("panic:", MAX_JOB_DETAIL_BYTES);
        let expected_panic = sanitize_detail(&format!("panicked: {panic_payload}"));
        let panic_id = manager.spawn("panic job", move |_| {
            std::panic::panic_any(panic_payload);
        });
        let panic_history = wait_history(&rx, panic_id);
        let panicked = panic_history.last().unwrap();
        assert_eq!(panicked.state, JobState::Failed);
        assert_eq!(panicked.detail, expected_panic);
        assert_one_line_bounded(&panicked.detail, MAX_JOB_DETAIL_BYTES);
        assert_eq!(sanitize_detail(&panicked.detail), panicked.detail);
    }

    #[test]
    fn thread_names_are_prefixed_and_truncated() {
        assert_eq!(thread_name("Export"), "job: Export");

        let long = thread_name("Transcribing a very long interview recording.mov");
        assert!(long.len() <= 32);
        assert!(long.starts_with("job: Transcribing"));

        // Multi-byte labels truncate on a char boundary, not mid-codepoint.
        let multibyte = thread_name("动画渲染动画渲染动画渲染动画渲染");
        assert!(multibyte.len() <= 32);

        // And the wiring: the job really runs under that name.
        let (manager, rx) = manager_with_events();
        let (name_tx, name_rx) = mpsc::channel();
        let id = manager.spawn("Exporting draft.mp4", move |_| {
            let name = thread::current().name().unwrap_or("").to_string();
            name_tx.send(name).unwrap();
            Ok(String::new())
        });
        assert_eq!(name_rx.recv().unwrap(), "job: Exporting draft.mp4");
        wait_history(&rx, id);
    }

    #[test]
    fn ids_are_unique_and_display_plainly() {
        assert_eq!(JobId::from_raw(7).raw(), 7);
        assert_eq!(JobId::from_raw(7).to_string(), "7");

        // Sequential spawn + drain: `wait_history` discards other jobs'
        // events, so concurrent jobs must not share one drain pass.
        let (manager, rx) = manager_with_events();
        let a = manager.spawn("a", |_| Ok(String::new()));
        wait_history(&rx, a);
        let b = manager.spawn("b", |_| Ok(String::new()));
        wait_history(&rx, b);
        assert_ne!(a, b);
    }
}
