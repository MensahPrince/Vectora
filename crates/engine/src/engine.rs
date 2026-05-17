//! [`Engine`] handle and [`EventReceiver`]: crossbeam channels and decode worker lifecycle.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};

use decoder::Rational;

use crate::command::EngineCommand;
use crate::event::EngineEvent;
use crate::ids::{RequestId, SourceId};
use crate::worker::run_worker;

/// Bounded capacity for the command queue (`Open`, `SeekExact`, …).
pub const CMD_CHANNEL_CAPACITY: usize = 16;
/// Bounded capacity for outgoing [`EngineEvent`]s — consumers should drain promptly.
pub const EVENT_CHANNEL_CAPACITY: usize = 4;
/// Scrub wakeups (`bounded(1)`): full means a wake is already pending.
pub const SCRUB_SIGNAL_CAPACITY: usize = 1;

/// Join the worker thread when this `Arc` is dropped — must run **after** [`Engine`]’s [`Sender`] fields
/// (the join guard is the **last** field on [`Engine`] so its `Drop` runs last).
struct WorkerJoin(Mutex<Option<JoinHandle<()>>>);

impl Drop for WorkerJoin {
    fn drop(&mut self) {
        let handle = match self.0.lock() {
            Ok(mut g) => g.take(),
            Err(e) => e.into_inner().take(),
        };
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

/// Cloneable handle to the decode worker: [`Sender`]s into **one** worker thread (see `docs/engine/research.md`).
///
/// # Threading
/// Safe to share across threads (`Send` + `Sync`). [`decoder::Decoder`] runs **only** on the worker.
///
/// # Shutdown
/// When the last [`Engine`] clone is dropped, struct fields are dropped in **declaration order**
/// ([`Sender`]s and counters first, join guard last) so command/scrub senders disconnect before the worker is joined.
///
/// # Panics
/// Command methods panic if the worker has already exited ([`Sender::send`] failure). Treat that as fatal.
pub struct Engine {
    tx_cmd: Sender<EngineCommand>,
    tx_scrub_signal: Sender<()>,
    scrub_slot: Arc<Mutex<Option<(SourceId, Rational)>>>,
    request_counter: Arc<AtomicU64>,
    next_source_id: Arc<AtomicU64>,
    join_on_drop: Arc<WorkerJoin>,
}

impl Clone for Engine {
    fn clone(&self) -> Self {
        Self {
            tx_cmd: self.tx_cmd.clone(),
            tx_scrub_signal: self.tx_scrub_signal.clone(),
            scrub_slot: Arc::clone(&self.scrub_slot),
            request_counter: Arc::clone(&self.request_counter),
            next_source_id: Arc::clone(&self.next_source_id),
            join_on_drop: Arc::clone(&self.join_on_drop),
        }
    }
}

/// Single-consumer receiver for [`EngineEvent`]. Clone **only** if you build your own fan-out; the MVP assumes one drainer.
pub struct EventReceiver(pub(crate) Receiver<EngineEvent>);

impl EventReceiver {
    pub fn recv(&self) -> Result<EngineEvent, crossbeam_channel::RecvError> {
        self.0.recv()
    }

    pub fn try_recv(&self) -> Result<EngineEvent, crossbeam_channel::TryRecvError> {
        self.0.try_recv()
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<EngineEvent, crossbeam_channel::RecvTimeoutError> {
        self.0.recv_timeout(timeout)
    }
}

impl Engine {
    /// Spawns the decode worker and returns handles for submitting work and receiving [`EngineEvent`]s.
    pub fn new() -> (Self, EventReceiver) {
        let (tx_cmd, rx_cmd) = bounded(CMD_CHANNEL_CAPACITY);
        let (tx_event, rx_event) = bounded(EVENT_CHANNEL_CAPACITY);
        let (tx_scrub_signal, rx_scrub_signal) = bounded(SCRUB_SIGNAL_CAPACITY);

        let scrub_slot = Arc::new(Mutex::new(None));
        let scrub_slot_worker = Arc::clone(&scrub_slot);

        let handle = std::thread::Builder::new()
            .name("cutlass-engine-decode".to_string())
            .spawn(move || {
                run_worker(rx_cmd, rx_scrub_signal, scrub_slot_worker, tx_event);
            })
            .expect("spawn cutlass-engine-decode thread");

        let join_on_drop = Arc::new(WorkerJoin(Mutex::new(Some(handle))));

        let engine = Self {
            tx_cmd,
            tx_scrub_signal,
            scrub_slot,
            request_counter: Arc::new(AtomicU64::new(0)),
            next_source_id: Arc::new(AtomicU64::new(1)),
            join_on_drop,
        };

        (engine, EventReceiver(rx_event))
    }

    fn send_cmd(&self, cmd: EngineCommand) {
        self.tx_cmd
            .send(cmd)
            .expect("decode worker disconnected unexpectedly");
    }

    fn next_request_id(&self) -> RequestId {
        RequestId(
            self.request_counter
                .fetch_add(1, Ordering::Relaxed),
        )
    }

    fn next_source_id(&self) -> SourceId {
        SourceId(
            self.next_source_id
                .fetch_add(1, Ordering::Relaxed),
        )
    }

    /// Opens `path`, assigning a new [`SourceId`]. MVP allows **one** open decoder; a second open yields
    /// [`EngineError::SourceAlreadyOpen`](crate::EngineError::SourceAlreadyOpen) on the event stream (see `docs/engine/research.md`).
    ///
    /// Returns `(source_id, request_id)` so callers can correlate the following [`EngineEvent::Opened`] or error.
    pub fn open(&self, path: PathBuf) -> (SourceId, RequestId) {
        let source_id = self.next_source_id();
        let request_id = self.next_request_id();
        self.send_cmd(EngineCommand::Open {
            source_id,
            path,
            request_id,
        });
        (source_id, request_id)
    }

    /// Exact presentation-time seek; matching [`EngineEvent::Frame`] / [`EngineEvent::Eof`] carry `request_id: Some(_)`.
    pub fn seek_exact(&self, source_id: SourceId, target: Rational) -> RequestId {
        let request_id = self.next_request_id();
        self.send_cmd(EngineCommand::SeekExact {
            source_id,
            target,
            request_id,
        });
        request_id
    }

    /// Next frame in decode order after the current demuxer position.
    pub fn next_frame(&self, source_id: SourceId) -> RequestId {
        let request_id = self.next_request_id();
        self.send_cmd(EngineCommand::NextFrame {
            source_id,
            request_id,
        });
        request_id
    }

    /// Removes `source_id` from the worker map (idempotent if already closed).
    pub fn close(&self, source_id: SourceId) {
        self.send_cmd(EngineCommand::Close { source_id });
    }

    /// Interactive scrub: coalesces rapid calls to **latest** `(source_id, target)` (see `docs/engine/research.md`).
    ///
    /// Emitted [`EngineEvent::Frame`] / [`EngineEvent::Eof`] use `request_id: None`.
    pub fn seek_scrub(&self, source_id: SourceId, target: Rational) {
        let mut slot = match self.scrub_slot.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        *slot = Some((source_id, target));
        drop(slot);
        let _ = self.tx_scrub_signal.try_send(());
    }
}
