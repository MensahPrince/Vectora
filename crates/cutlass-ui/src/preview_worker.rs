//! Background preview rendering: engine and decode/composite stay off the UI thread.

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use cutlass_commands::{Command, EditCommand, EditOutcome, ProjectCommand};
use cutlass_engine::{ApplyOutcome, Engine, EngineConfig};
use cutlass_models::{
    Rational, RationalTime, TimeRange, TrackKind,
};
use tracing::{error, info};

use crate::PreviewStore;

pub struct PreviewSession {
    pub duration_ticks: i64,
    pub tl_rate: Rational,
}

pub struct PreviewWorker {
    req_tx: Sender<i64>,
    _handle: JoinHandle<()>,
}

impl PreviewWorker {
    /// Spawns a dedicated thread that owns the [`Engine`] (required: decoders are not `Send`).
    pub fn spawn(
        config: EngineConfig,
        media_path: PathBuf,
        preview_weak: slint::Weak<PreviewStore<'static>>,
    ) -> Result<(Self, PreviewSession), String> {
        let (ready_tx, ready_rx) = bounded(1);
        let (req_tx, req_rx) = unbounded();

        let handle = std::thread::Builder::new()
            .name("cutlass-preview".into())
            .spawn(move || {
                if let Err(e) = worker_main(config, &media_path, preview_weak, req_rx, ready_tx) {
                    error!("preview worker exited: {e}");
                }
            })
            .map_err(|e| e.to_string())?;

        let session = ready_rx
            .recv()
            .map_err(|e| e.to_string())?
            .map_err(|e: String| e)?;

        Ok((Self { req_tx, _handle: handle }, session))
    }

    pub fn request_frame(&self, tick: i64) {
        let _ = self.req_tx.send(tick);
    }
}

fn worker_main(
    config: EngineConfig,
    media_path: &Path,
    preview_weak: slint::Weak<PreviewStore<'static>>,
    req_rx: Receiver<i64>,
    ready_tx: Sender<Result<PreviewSession, String>>,
) -> Result<(), String> {
    let mut engine = Engine::new(config).map_err(|e| e.to_string())?;
    let session = bootstrap_timeline(&mut engine, media_path)?;
    info!(
        duration_ticks = session.duration_ticks,
        tl_rate = ?session.tl_rate,
        "preview worker ready"
    );
    let tl_rate = session.tl_rate;
    ready_tx
        .send(Ok(session))
        .map_err(|e| e.to_string())?;

    preview_loop(&mut engine, tl_rate, preview_weak, req_rx);
    Ok(())
}

fn bootstrap_timeline(engine: &mut Engine, media_path: &Path) -> Result<PreviewSession, String> {
    let media_id = match engine.apply(Command::Project(ProjectCommand::Import {
        path: media_path.to_path_buf(),
    })) {
        Ok(ApplyOutcome::Imported { media }) => media,
        Ok(other) => {
            return Err(format!("import failed for {}: {other:?}", media_path.display()));
        }
        Err(e) => return Err(e.to_string()),
    };

    let track_id = match engine.apply(Command::Edit(EditCommand::AddTrack {
        kind: TrackKind::Video,
        name: "V1".into(),
    })) {
        Ok(ApplyOutcome::Edited(EditOutcome::CreatedTrack(id))) => id,
        Ok(other) => return Err(format!("add track failed: {other:?}")),
        Err(e) => return Err(e.to_string()),
    };

    let (clip_len, media_rate) = engine
        .project()
        .media(media_id)
        .map(|media| (media.duration.value, media.frame_rate))
        .ok_or_else(|| "imported media missing from pool".to_string())?;

    let tl_rate = engine.project().timeline().frame_rate;
    match engine.apply(Command::Edit(EditCommand::AddClip {
        track: track_id,
        media: media_id,
        source: TimeRange::at_rate(0, clip_len, media_rate),
        start: RationalTime::new(0, tl_rate),
    })) {
        Ok(ApplyOutcome::Edited(EditOutcome::Created(_))) => {}
        Ok(other) => return Err(format!("add clip failed: {other:?}")),
        Err(e) => return Err(e.to_string()),
    }

    Ok(PreviewSession {
        duration_ticks: engine.project().timeline().duration().value,
        tl_rate,
    })
}

fn preview_loop(
    engine: &mut Engine,
    tl_rate: Rational,
    preview_weak: slint::Weak<PreviewStore<'static>>,
    req_rx: Receiver<i64>,
) {
    while let Ok(mut tick) = req_rx.recv() {
        while let Ok(latest) = req_rx.try_recv() {
            tick = latest;
        }

        match engine.get_frame(RationalTime::new(tick, tl_rate)) {
            Ok(frame) => {
                let weak = preview_weak.clone();
                if let Err(e) = slint::invoke_from_event_loop(move || {
                    if let Some(store) = weak.upgrade() {
                        store.set_frame(crate::preview::to_slint_image(frame));
                    }
                }) {
                    error!("failed to deliver preview frame to UI: {e}");
                }
            }
            Err(e) => error!(tick, "preview frame failed: {e}"),
        }
    }
}
