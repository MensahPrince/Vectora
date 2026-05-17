//! Decode worker thread: owns [`decoder::Decoder`] map and drives FFmpeg **only** here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};
use crossbeam_channel::select;

use decoder::{DecodeOutcome, Decoder, Rational};

use crate::command::EngineCommand;
use crate::event::EngineEvent;
use crate::error::EngineError;
use crate::ids::SourceId;

pub(crate) fn run_worker(
    rx_cmd: Receiver<EngineCommand>,
    rx_scrub_signal: Receiver<()>,
    scrub_slot: Arc<Mutex<Option<(SourceId, Rational)>>>,
    tx_event: Sender<EngineEvent>,
) {
    let mut decoders: HashMap<SourceId, Decoder> = HashMap::new();

    loop {
        select! {
            recv(rx_cmd) -> msg => {
                match msg {
                    Ok(cmd) => handle_command(cmd, &mut decoders, &tx_event),
                    Err(_) => break,
                }
            }
            recv(rx_scrub_signal) -> _ => {
                // Drain queued commands first so a prior `seek_exact` isn't reordered after `seek_scrub`
                // when both arrive in the same UI tick (see `docs/engine/research.md`).
                while let Ok(cmd) = rx_cmd.try_recv() {
                    handle_command(cmd, &mut decoders, &tx_event);
                }
                while rx_scrub_signal.try_recv().is_ok() {}
                let slot = match scrub_slot.lock() {
                    Ok(mut g) => g.take(),
                    Err(poisoned) => poisoned.into_inner().take(),
                };
                if let Some((sid, target)) = slot {
                    handle_scrub(sid, target, &mut decoders, &tx_event);
                }
            }
        }
    }
}

fn send_event(tx: &Sender<EngineEvent>, event: EngineEvent) {
    let _ = tx.send(event);
}

fn handle_scrub(
    source_id: SourceId,
    target: Rational,
    decoders: &mut HashMap<SourceId, Decoder>,
    tx_event: &Sender<EngineEvent>,
) {
    let Some(dec) = decoders.get_mut(&source_id) else {
        send_event(
            tx_event,
            EngineEvent::Error {
                source_id: Some(source_id),
                error: EngineError::SourceNotFound(source_id),
                request_id: None,
            },
        );
        return;
    };

    match dec.seek_scrub(target) {
        Ok(DecodeOutcome::Frame(frame)) => send_event(
            tx_event,
            EngineEvent::Frame {
                source_id,
                frame,
                request_id: None,
            },
        ),
        Ok(DecodeOutcome::Eof) => send_event(
            tx_event,
            EngineEvent::Eof {
                source_id,
                request_id: None,
            },
        ),
        Err(e) => send_event(
            tx_event,
            EngineEvent::Error {
                source_id: Some(source_id),
                error: e.into(),
                request_id: None,
            },
        ),
    }
}

fn handle_command(
    cmd: EngineCommand,
    decoders: &mut HashMap<SourceId, Decoder>,
    tx_event: &Sender<EngineEvent>,
) {
    match cmd {
        EngineCommand::Open {
            source_id,
            path,
            request_id,
        } => {
            if let Some((&existing_id, _)) = decoders.iter().next() {
                // MVP: one decoder only — reject a second `SourceId` or duplicate `Open`.
                send_event(
                    tx_event,
                    EngineEvent::Error {
                        source_id: Some(source_id),
                        error: EngineError::SourceAlreadyOpen(existing_id),
                        request_id: Some(request_id),
                    },
                );
                return;
            }

            match Decoder::open(&path) {
                Ok(decoder) => {
                    let info = decoder.info().clone();
                    decoders.insert(source_id, decoder);
                    send_event(
                        tx_event,
                        EngineEvent::Opened {
                            source_id,
                            info,
                            request_id,
                        },
                    );
                }
                Err(e) => send_event(
                    tx_event,
                    EngineEvent::Error {
                        source_id: Some(source_id),
                        error: e.into(),
                        request_id: Some(request_id),
                    },
                ),
            }
        }
        EngineCommand::SeekExact {
            source_id,
            target,
            request_id,
        } => {
            let Some(dec) = decoders.get_mut(&source_id) else {
                send_event(
                    tx_event,
                    EngineEvent::Error {
                        source_id: Some(source_id),
                        error: EngineError::SourceNotFound(source_id),
                        request_id: Some(request_id),
                    },
                );
                return;
            };

            match dec.seek_exact(target) {
                Ok(DecodeOutcome::Frame(frame)) => send_event(
                    tx_event,
                    EngineEvent::Frame {
                        source_id,
                        frame,
                        request_id: Some(request_id),
                    },
                ),
                Ok(DecodeOutcome::Eof) => send_event(
                    tx_event,
                    EngineEvent::Eof {
                        source_id,
                        request_id: Some(request_id),
                    },
                ),
                Err(e) => send_event(
                    tx_event,
                    EngineEvent::Error {
                        source_id: Some(source_id),
                        error: e.into(),
                        request_id: Some(request_id),
                    },
                ),
            }
        }
        EngineCommand::NextFrame {
            source_id,
            request_id,
        } => {
            let Some(dec) = decoders.get_mut(&source_id) else {
                send_event(
                    tx_event,
                    EngineEvent::Error {
                        source_id: Some(source_id),
                        error: EngineError::SourceNotFound(source_id),
                        request_id: Some(request_id),
                    },
                );
                return;
            };

            match dec.next_frame() {
                Ok(DecodeOutcome::Frame(frame)) => send_event(
                    tx_event,
                    EngineEvent::Frame {
                        source_id,
                        frame,
                        request_id: Some(request_id),
                    },
                ),
                Ok(DecodeOutcome::Eof) => send_event(
                    tx_event,
                    EngineEvent::Eof {
                        source_id,
                        request_id: Some(request_id),
                    },
                ),
                Err(e) => send_event(
                    tx_event,
                    EngineEvent::Error {
                        source_id: Some(source_id),
                        error: e.into(),
                        request_id: Some(request_id),
                    },
                ),
            }
        }
        EngineCommand::Close { source_id } => {
            if decoders.remove(&source_id).is_some() {
                send_event(tx_event, EngineEvent::Closed { source_id });
            }
        }
    }
}
