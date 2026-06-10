//! Engine session on a background thread: commands, preview frames, snapshots.

use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use cutlass_commands::{Command, EditCommand};
use cutlass_engine::{ColorConvertPath, Engine, EngineConfig, RgbaFrame};
use cutlass_models::RationalTime;
use slint::ComponentHandle;
use tracing::{error, warn};

use crate::bootstrap;
use crate::ids::{clip_id_from_str, clip_id_to_str, track_id_from_str, track_id_to_str};
use crate::snapshot::{FrameSnapshot, ProjectSnapshot};

const CACHE_BUDGET: u64 = 256 * 1024 * 1024;

pub enum EngineRequest {
    MoveClip {
        source_track_id: String,
        clip_id: String,
        target_track_id: String,
        new_start_value: i32,
    },
    RequestFrame {
        tick: i32,
        generation: u64,
    },
    Shutdown,
}

pub enum EngineEvent {
    /// Initial load or structural replace (open, import, undo, …).
    Project(ProjectSnapshot),
    /// In-place reposition on one lane.
    ClipMoved {
        track_id: String,
        clip_id: String,
        new_start_value: i32,
    },
    /// Clip removed from `source_track_id` and appended on `target_track_id`.
    ClipTransferred {
        source_track_id: String,
        target_track_id: String,
        clip_id: String,
        new_start_value: i32,
    },
    Frame(FrameSnapshot),
    MoveRejected(String),
}

pub struct EngineHandle {
    tx: Sender<EngineRequest>,
    pub events: Receiver<EngineEvent>,
    _join: JoinHandle<()>,
}

impl EngineHandle {
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = bounded(32);
        let (evt_tx, evt_rx) = unbounded();

        let join = thread::Builder::new()
            .name("cutlass-engine".into())
            .spawn(move || run_session(req_rx, evt_tx))
            .expect("spawn engine thread");

        Self {
            tx: req_tx,
            events: evt_rx,
            _join: join,
        }
    }

    pub fn move_clip(
        &self,
        source_track_id: &str,
        clip_id: &str,
        target_track_id: &str,
        new_start_value: i32,
    ) {
        let _ = self.tx.send(EngineRequest::MoveClip {
            source_track_id: source_track_id.to_owned(),
            clip_id: clip_id.to_owned(),
            target_track_id: target_track_id.to_owned(),
            new_start_value,
        });
    }

    pub fn request_frame(&self, tick: i32, generation: u64) {
        let _ = self.tx.send(EngineRequest::RequestFrame { tick, generation });
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(EngineRequest::Shutdown);
    }
}

fn run_session(req_rx: Receiver<EngineRequest>, evt_tx: Sender<EngineEvent>) {
    cutlass_engine::init();

    let dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(err) => {
            error!("engine tempdir: {err}");
            return;
        }
    };

    let config = EngineConfig {
        cache_dir: dir.path().join("cache"),
        cache_budget_bytes: CACHE_BUDGET,
        undo_limit: 64,
        color_convert: ColorConvertPath::Gpu,
    };

    let mut engine = match Engine::new(config) {
        Ok(e) => e,
        Err(err) => {
            error!("engine init: {err}");
            return;
        }
    };

    if let Err(err) = bootstrap::bootstrap_demo(&mut engine) {
        warn!("bootstrap demo: {err}");
    }

    let _ = evt_tx.send(EngineEvent::Project(ProjectSnapshot::from_engine(
        engine.project(),
    )));

    while let Ok(msg) = req_rx.recv() {
        match msg {
            EngineRequest::Shutdown => break,
            EngineRequest::MoveClip {
                source_track_id,
                clip_id,
                target_track_id,
                new_start_value,
            } => handle_move_clip(
                &mut engine,
                &evt_tx,
                &source_track_id,
                &clip_id,
                &target_track_id,
                new_start_value,
            ),
            EngineRequest::RequestFrame { tick, generation } => {
                let mut latest = (tick, generation);
                while let Ok(EngineRequest::RequestFrame { tick, generation }) = req_rx.try_recv() {
                    latest = (tick, generation);
                }
                handle_frame(&mut engine, &evt_tx, latest.0, latest.1);
            }
        }
    }
}

fn handle_move_clip(
    engine: &mut Engine,
    evt_tx: &Sender<EngineEvent>,
    source_track_id: &str,
    clip_id: &str,
    target_track_id: &str,
    new_start_value: i32,
) {
    let Some(clip) = clip_id_from_str(clip_id) else {
        let _ = evt_tx.send(EngineEvent::MoveRejected(format!("invalid clip id {clip_id:?}")));
        return;
    };
    let Some(to_track) = track_id_from_str(target_track_id) else {
        let _ = evt_tx.send(EngineEvent::MoveRejected(format!(
            "invalid target track {target_track_id:?}"
        )));
        return;
    };

    let from_track = engine.project().timeline().track_of(clip);

    let rate = engine.project().timeline().frame_rate;
    let start = RationalTime::new(i64::from(new_start_value), rate);

    match engine.apply(Command::Edit(EditCommand::MoveClip {
        clip,
        to_track,
        start,
    })) {
        Ok(_) => {
            let Some(to_track_id) = engine.project().timeline().track_of(clip) else {
                let _ = evt_tx.send(EngineEvent::MoveRejected(format!(
                    "clip {clip_id:?} missing after move"
                )));
                return;
            };
            let Some(updated) = engine.project().clip(clip) else {
                let _ = evt_tx.send(EngineEvent::MoveRejected(format!(
                    "clip {clip_id:?} missing after move"
                )));
                return;
            };
            let committed_start = updated.timeline.start.value as i32;
            let clip_id_str = clip_id_to_str(clip);
            let to_track_str = track_id_to_str(to_track_id);

            let event = match from_track {
                Some(from) if from == to_track_id => EngineEvent::ClipMoved {
                    track_id: to_track_str,
                    clip_id: clip_id_str,
                    new_start_value: committed_start,
                },
                Some(from) => EngineEvent::ClipTransferred {
                    source_track_id: track_id_to_str(from),
                    target_track_id: to_track_str,
                    clip_id: clip_id_str,
                    new_start_value: committed_start,
                },
                None => {
                    let _ = evt_tx.send(EngineEvent::MoveRejected(format!(
                        "clip {clip_id:?} had no source track"
                    )));
                    return;
                }
            };
            let _ = evt_tx.send(event);
        }
        Err(err) => {
            let _ = evt_tx.send(EngineEvent::MoveRejected(err.to_string()));
        }
    }

    let _ = source_track_id;
}

fn handle_frame(engine: &mut Engine, evt_tx: &Sender<EngineEvent>, tick: i32, generation: u64) {
    let rate = engine.project().timeline().frame_rate;
    let time = RationalTime::new(i64::from(tick), rate);

    match engine.get_frame(time) {
        Ok(frame) => {
            if let Some(snapshot) = frame_to_snapshot(&frame, generation) {
                let _ = evt_tx.send(EngineEvent::Frame(snapshot));
            }
        }
        Err(err) => {
            tracing::debug!("preview frame @ {tick}: {err}");
        }
    }
}

fn frame_to_snapshot(frame: &RgbaFrame, generation: u64) -> Option<FrameSnapshot> {
    let width = frame.width;
    let height = frame.height;
    if width == 0 || height == 0 {
        return None;
    }
    let needed = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    if frame.bytes.len() < needed {
        return None;
    }
    Some(FrameSnapshot {
        generation,
        width,
        height,
        rgba: frame.bytes[..needed].to_vec(),
    })
}

fn frame_to_image(frame: &FrameSnapshot) -> Option<slint::Image> {
    use slint::{Image, SharedPixelBuffer, Rgba8Pixel};
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &frame.rgba,
        frame.width,
        frame.height,
    );
    Some(Image::from_rgba8(buffer))
}

/// Poll engine events and push them into Slint globals.
pub fn drain_events(
    engine: &EngineHandle,
    app: &crate::AppWindow,
    projector: &std::cell::RefCell<crate::projector::Projector>,
    last_frame_generation: &mut u64,
) {
    while let Ok(event) = engine.events.try_recv() {
        match event {
            EngineEvent::Project(snapshot) => {
                projector.borrow_mut().rebuild_from_snapshot(&snapshot);
                app.global::<crate::EditorStore>()
                    .set_project(projector.borrow().slint_project().clone());
            }
            EngineEvent::ClipMoved {
                track_id,
                clip_id,
                new_start_value,
            } => {
                if !projector.borrow().move_clip(&track_id, &clip_id, new_start_value) {
                    warn!(
                        "incremental move_clip patch failed for clip {clip_id} on track {track_id}"
                    );
                }
            }
            EngineEvent::ClipTransferred {
                source_track_id,
                target_track_id,
                clip_id,
                new_start_value,
            } => {
                if !projector.borrow_mut().transfer_clip(
                    &source_track_id,
                    &target_track_id,
                    &clip_id,
                    new_start_value,
                ) {
                    warn!(
                        "incremental transfer_clip patch failed: {clip_id} \
                         {source_track_id} → {target_track_id}"
                    );
                }
            }
            EngineEvent::Frame(frame) => {
                if frame.generation >= *last_frame_generation {
                    *last_frame_generation = frame.generation;
                    if let Some(image) = frame_to_image(&frame) {
                        let preview = app.global::<crate::PreviewStore>();
                        preview.set_frame(image);
                        preview.set_has_frame(true);
                    }
                }
            }
            EngineEvent::MoveRejected(msg) => {
                warn!("move-clip rejected: {msg}");
            }
        }
    }
}

/// Timer-driven pump for engine events.
pub fn install_event_pump(
    app: &crate::AppWindow,
    engine: std::rc::Rc<EngineHandle>,
    projector: std::rc::Rc<std::cell::RefCell<crate::projector::Projector>>,
    frame_generation: std::rc::Rc<std::cell::Cell<u64>>,
) {
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let mut last = frame_generation.get();
        drain_events(&engine, &app, &projector, &mut last);
        frame_generation.set(last);
    });
}
