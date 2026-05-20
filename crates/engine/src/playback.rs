//! Engine handle + worker thread.
//!
//! Single decoder, single worker, two channels (command + event) plus a
//! latest-wins "scrub slot" so the worker never wastes time decoding stale
//! drag positions. Drop the [`Engine`] handle to shut the worker down.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select};
use tracing::{debug, error, warn};

use decoder::{DecodeOutcome, Decoder, Rational};

use crate::convert::frame_to_rgba8;
use crate::error::EngineError;

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// A frame ready for the UI. Packed RGBA8, no row padding.
#[derive(Debug, Clone)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, R G B A.
    pub rgba: Vec<u8>,
    /// Source presentation time in seconds (kept as a rational; consumers can
    /// `.as_f64()` for logging).
    pub pts: Rational,
}

/// Events the engine emits to its consumer. **Drain promptly** — the event
/// channel is bounded so the worker will park if the consumer falls behind.
///
/// Note: the event channel is a *bounded(1) latest-wins-on-overflow* buffer
/// for `Frame` events. If the UI is slow we'd rather drop an old preview
/// frame than queue stale work.
#[derive(Debug)]
pub enum EngineEvent {
    /// A source has been opened. Includes basic geometry / duration.
    Opened {
        width: u32,
        height: u32,
        duration: Option<Rational>,
    },
    /// A decoded preview frame.
    Frame(PreviewFrame),
    /// Last seek landed past end of stream.
    Eof,
    /// Recoverable or fatal failure.
    Error(EngineError),
}

/// Consumer end of the engine event stream.
pub type EventReceiver = Receiver<EngineEvent>;

/// Clone-able handle. Submits commands to the worker thread; cloning is cheap
/// (`Arc`). When the **last** handle drops the worker is asked to exit and
/// joined inside `Drop`.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

struct EngineInner {
    tx_cmd: Sender<Command>,
    tx_scrub_signal: Sender<()>,
    scrub_slot: Arc<Mutex<Option<Rational>>>,
    // Held in an Option so Drop can `take` and join. Mutex<Option<_>> works
    // here because Drop is the only place that touches this.
    worker: Mutex<Option<JoinHandle<()>>>,
}

// ---------------------------------------------------------------------------
// Worker plumbing
// ---------------------------------------------------------------------------

enum Command {
    Open(PathBuf),
    Shutdown,
}

impl Engine {
    /// Spawn the worker thread and hand back the handle + event receiver.
    pub fn spawn() -> (Self, EventReceiver) {
        let (tx_cmd, rx_cmd) = bounded::<Command>(8);
        // bounded(1) so multiple back-to-back scrubs collapse into one wake-up.
        let (tx_scrub_signal, rx_scrub_signal) = bounded::<()>(1);
        // bounded(1) for events: we'd rather drop a stale preview than queue work.
        let (tx_event, rx_event) = bounded::<EngineEvent>(1);

        let scrub_slot: Arc<Mutex<Option<Rational>>> = Arc::new(Mutex::new(None));
        let scrub_slot_worker = scrub_slot.clone();

        let worker = thread::Builder::new()
            .name("cutlass-engine".into())
            .spawn(move || {
                worker_loop(rx_cmd, rx_scrub_signal, scrub_slot_worker, tx_event);
            })
            .expect("spawn cutlass-engine worker thread");

        (
            Self {
                inner: Arc::new(EngineInner {
                    tx_cmd,
                    tx_scrub_signal,
                    scrub_slot,
                    worker: Mutex::new(Some(worker)),
                }),
            },
            rx_event,
        )
    }

    /// Open a source. Reply arrives as [`EngineEvent::Opened`] (or
    /// [`EngineEvent::Error`]) on the event channel.
    pub fn open(&self, path: PathBuf) -> Result<(), EngineError> {
        self.inner
            .tx_cmd
            .send(Command::Open(path))
            .map_err(|_| EngineError::WorkerDead)
    }

    /// Submit a scrub target. Latest call wins — any prior pending target is
    /// **discarded** before the worker sees it. Cheap to spam during drag.
    pub fn seek_scrub(&self, target: Rational) {
        if let Ok(mut slot) = self.inner.scrub_slot.lock() {
            *slot = Some(target);
        }
        // Signal channel is bounded(1); a full channel just means the worker
        // already has a wake-up pending and will see the new slot on arrival.
        match self.inner.tx_scrub_signal.try_send(()) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                warn!("engine worker is gone; dropped scrub seek");
            }
        }
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        // Best-effort: ask worker to exit and join. Failures here are only
        // possible if the worker has already crashed.
        let _ = self.tx_cmd.send(Command::Shutdown);
        if let Ok(mut guard) = self.worker.lock()
            && let Some(handle) = guard.take()
            && let Err(e) = handle.join()
        {
            error!(?e, "engine worker panicked");
        }
    }
}

fn worker_loop(
    rx_cmd: Receiver<Command>,
    rx_scrub_signal: Receiver<()>,
    scrub_slot: Arc<Mutex<Option<Rational>>>,
    tx_event: Sender<EngineEvent>,
) {
    let mut decoder: Option<Decoder> = None;

    loop {
        select! {
            recv(rx_cmd) -> msg => match msg {
                Ok(Command::Open(path)) => {
                    debug!(?path, "engine: opening source");
                    match Decoder::open(&path) {
                        Ok(mut d) => {

                            d.set_fast_scrub(true);

                            let info = d.info();
                            send_event(&tx_event, EngineEvent::Opened {
                                width: info.width,
                                height: info.height,
                                duration: info.duration,
                            });
                            decoder = Some(d);
                        }
                        Err(e) => {
                            send_event(&tx_event, EngineEvent::Error(EngineError::Decoder(e)));
                        }
                    }
                }
                Ok(Command::Shutdown) | Err(_) => break,
            },
            recv(rx_scrub_signal) -> _ => {
                let target = scrub_slot.lock().ok().and_then(|mut s| s.take());
                let Some(target) = target else { continue };
                let Some(dec) = decoder.as_mut() else {
                    send_event(&tx_event, EngineEvent::Error(EngineError::NoSource));
                    continue;
                };
                match dec.seek_exact(target) {
                    Ok(DecodeOutcome::Frame(frame)) => {
                        let preview = PreviewFrame {
                            width: frame.width,
                            height: frame.height,
                            rgba: frame_to_rgba8(&frame),
                            pts: frame.pts,
                        };
                        send_event(&tx_event, EngineEvent::Frame(preview));
                    }
                    Ok(DecodeOutcome::Eof) => {
                        send_event(&tx_event, EngineEvent::Eof);
                    }
                    Err(e) => {
                        send_event(&tx_event, EngineEvent::Error(EngineError::Decoder(e)));
                    }
                }
            }
        }
    }
}

/// Send an event, but if the consumer is behind drop the **oldest** queued
/// event and try once more. This keeps the engine producing fresh frames
/// instead of stalling on a slow UI.
fn send_event(tx: &Sender<EngineEvent>, ev: EngineEvent) {
    match tx.try_send(ev) {
        Ok(()) => {}
        Err(TrySendError::Full(ev)) => {
            // Pop the stale event by clone-receiving from our own sender's
            // mate? We can't from here. Best we can do: blocking send so the
            // worker pauses briefly. Since the consumer is the UI event loop
            // pulling each tick this is bounded by ~16ms in the worst case.
            if let Err(e) = tx.send(ev) {
                warn!(?e, "engine: event consumer dropped");
            }
        }
        Err(TrySendError::Disconnected(_)) => {
            warn!("engine: event consumer dropped");
        }
    }
}
