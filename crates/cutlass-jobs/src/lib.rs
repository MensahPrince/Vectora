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
//! - **Cancellation is cooperative, and the flag decides.** [`JobManager::cancel`]
//!   flips a flag; the job observes it via [`JobContext::cancelled`] and
//!   returns when convenient. Whatever it returns after that — `Ok` or `Err`
//!   — the terminal state is [`JobState::Cancelled`], with the returned
//!   string kept as the final detail. A job that never checks simply runs to
//!   completion and is then marked cancelled anyway.
//! - **Subscribers are called with no lock held.** Every transition collects
//!   the snapshot and the subscriber list under the lock, drops the guard,
//!   then calls — so callbacks may re-enter the registry (query, spawn,
//!   cancel) without deadlocking. The `Queued` notification runs on the
//!   spawning thread, everything after on the job's own thread; because
//!   `Queued` is delivered before the thread starts, a subscriber sees each
//!   job strictly in lifecycle order: Queued → Running → progress… →
//!   terminal.
//! - **`Instant`, not `SystemTime`.** Timestamps exist to render elapsed
//!   time and to order pruning — both monotonic concerns; wall-clock time
//!   can jump under NTP. Nothing here persists across runs, so wall time
//!   buys nothing.
//! - **Bounded.** Finished jobs beyond the newest [`KEEP_FINISHED`] are
//!   pruned (by *finish* time — a long export that just ended is recent
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
    /// Human label ("Exporting draft.mp4", "Transcribing interview.mov").
    pub label: String,
    pub state: JobState,
    /// `0.0..=1.0` when the job reports progress; `None` for indeterminate.
    /// Snaps to `Some(1.0)` on [`JobState::Done`] if the job ever reported;
    /// Failed/Cancelled keep the last reported value.
    pub progress: Option<f32>,
    /// One-line current detail ("frame 1200/3600"), the failure reason after
    /// [`JobState::Failed`], or the result summary after [`JobState::Done`].
    pub detail: String,
    /// A cancel was requested but the job hasn't exited yet — render as
    /// "cancelling…". Stays set on the terminal snapshot of cancelled jobs.
    pub cancel_requested: bool,
    /// When [`spawn`](JobManager::spawn) registered the job (threads start
    /// immediately, so this is run start for practical purposes).
    pub started: Instant,
    pub finished: Option<Instant>,
}

/// Handle a running job uses to report progress and to notice cancellation.
/// Deliberately not `Clone` and only lent to the work closure, so progress
/// can never arrive from a job that already returned.
pub struct JobContext {
    id: JobId,
    shared: Arc<Shared>,
    cancel: Arc<AtomicBool>,
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
        let detail = detail.into();
        self.shared.update(self.id, |job| {
            job.snap.progress = Some(fraction);
            job.snap.detail = detail;
        });
    }

    /// True once [`JobManager::cancel`] was called for this job. Lock-free —
    /// check it as often as the inner loop likes. `Relaxed` is enough: this
    /// is a pure flag, and the registry mutex orders everything else.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
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
    /// closure's `Ok(summary)` / `Err(reason)` becomes the final detail. If
    /// the job was cancelled before it returned, the state is `Cancelled`
    /// regardless of the return — the flag decides, not the value.
    pub fn spawn(
        &self,
        label: impl Into<String>,
        work: impl FnOnce(&JobContext) -> Result<String, String> + Send + 'static,
    ) -> JobId {
        let label = label.into();
        let id = JobId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));

        let (snapshot, subscribers) = {
            let mut inner = self.shared.inner.lock().expect("job registry lock");
            let snap = JobSnapshot {
                id,
                label: label.clone(),
                state: JobState::Queued,
                progress: None,
                detail: String::new(),
                cancel_requested: false,
                started: Instant::now(),
                finished: None,
            };
            inner.jobs.push(Job {
                snap: snap.clone(),
                cancel: cancel.clone(),
            });
            (snap, inner.subscribers.clone())
        };
        // Deliver Queued before the thread exists, so a subscriber's event
        // stream for this job can never open with Running.
        for subscriber in &subscribers {
            subscriber(snapshot.clone());
        }

        let context = JobContext {
            id,
            shared: Arc::clone(&self.shared),
            cancel,
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
                        format!("panicked: {}", panic_message(&*payload)),
                        true,
                    ),
                    Ok(Ok(summary)) => (JobState::Done, summary, false),
                    Ok(Err(reason)) => (JobState::Failed, reason, false),
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
            self.shared.update(id, |job| {
                job.snap.state = JobState::Failed;
                job.snap.detail = format!("failed to spawn thread: {err}");
                job.snap.finished = Some(Instant::now());
            });
        }
        id
    }

    /// Cooperative cancel: flips the job's flag; the job exits at its next
    /// [`JobContext::cancelled`] check. Returns `false` for unknown or
    /// already-finished jobs.
    ///
    /// No subscriber notification fires here — the request doesn't change
    /// the job's state, and an out-of-band event from the canceller's thread
    /// could be delivered after the job's own terminal event and wedge a UI
    /// on a stale "cancelling" row. The request is visible immediately via
    /// [`get`](Self::get) / [`snapshot`](Self::snapshot)
    /// (`cancel_requested`), and to subscribers in the eventual `Cancelled`
    /// terminal event.
    pub fn cancel(&self, id: JobId) -> bool {
        let mut inner = self.shared.inner.lock().expect("job registry lock");
        let Some(index) = inner.index_of(id) else {
            return false;
        };
        let job = &mut inner.jobs[index];
        if job.snap.state.is_terminal() {
            return false;
        }
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
    /// with **no registry lock held**, in per-job lifecycle order. Keep them
    /// cheap — post into an event loop rather than doing work inline. To
    /// catch up on pre-existing jobs, subscribe first, then render one
    /// [`snapshot`](Self::snapshot).
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
}

impl Shared {
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
            let snapshot = job.snap.clone();
            if snapshot.state.is_terminal() {
                inner.prune_finished();
            }
            (snapshot, inner.subscribers.clone())
        };
        for subscriber in &subscribers {
            subscriber(snapshot.clone());
        }
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

/// Worker threads are named so profilers, debuggers, and crash reports
/// attribute the work. Capped at 32 bytes on a char boundary — platforms
/// impose their own hard limits (15 bytes on Linux), we just keep names tidy
/// and valid UTF-8 ourselves.
fn thread_name(label: &str) -> String {
    const MAX_LEN: usize = 32;
    let mut name = format!("job: {label}");
    if name.len() > MAX_LEN {
        let mut end = MAX_LEN;
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
            events.push(snapshot);
            if terminal {
                return events;
            }
        }
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
