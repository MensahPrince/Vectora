mod bootstrap;
mod ids;
mod palette;
mod projector;
mod ruler;
mod session;
mod snap;
mod snapshot;
mod timecode;
mod timeline;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::ComponentHandle;
use slint::SharedString;
use slint::BackendSelector;
use slint::wgpu_28::WGPUConfiguration;
use tracing_subscriber::EnvFilter;

use crate::projector::Projector;
use crate::session::{EngineEvent, EngineHandle, drain_events, install_event_pump};
use crate::snapshot::ProjectSnapshot;

slint::include_modules!();

fn setup_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn main() -> Result<(), slint::PlatformError> {
    setup_tracing();
    BackendSelector::new()
        .require_wgpu_28(WGPUConfiguration::default())
        .select()?;

    let app = AppWindow::new()?;
    let engine = Rc::new(EngineHandle::spawn());
    let frame_generation = Rc::new(Cell::new(0u64));

    let initial_snapshot = loop {
        match engine.events.recv() {
            Ok(EngineEvent::Project(snapshot)) => break snapshot,
            Ok(EngineEvent::MoveRejected(msg)) => {
                eprintln!("engine bootstrap error: {msg}");
            }
            Ok(EngineEvent::Frame(_))
            | Ok(EngineEvent::ClipMoved { .. })
            | Ok(EngineEvent::ClipTransferred { .. }) => {}
            Err(_) => {
                eprintln!("engine thread exited before publishing a project");
                break ProjectSnapshot::from_engine(&cutlass_models::Project::new(
                    "empty",
                    cutlass_models::Rational::FPS_24,
                ));
            }
        }
    };

    let projector = Rc::new(RefCell::new(Projector::from_snapshot(&initial_snapshot)));
    app.global::<EditorStore>()
        .set_project(projector.borrow().slint_project().clone());

    let generation = frame_generation.get() + 1;
    frame_generation.set(generation);
    engine.request_frame(0, generation);

    install_event_pump(
        &app,
        engine.clone(),
        projector.clone(),
        frame_generation.clone(),
    );

    {
        let engine = engine.clone();
        let frame_generation = frame_generation.clone();
        let weak = app.as_weak();
        let projector = projector.clone();
        app.global::<TimelineStore>().on_playhead_changed(move |tick| {
            let generation = frame_generation.get() + 1;
            frame_generation.set(generation);
            engine.request_frame(tick, generation);
            if let Some(app) = weak.upgrade() {
                let mut last = 0u64;
                drain_events(&engine, &app, &projector, &mut last);
            }
        });
    }

    {
        let engine = engine.clone();
        let weak = app.as_weak();
        let projector = projector.clone();
        app.global::<EditorStore>().on_move_clip(
            move |source_track_id: SharedString,
                  clip_id: SharedString,
                  target_track_id: SharedString,
                  new_start_value: i32| {
                engine.move_clip(
                    source_track_id.as_str(),
                    clip_id.as_str(),
                    target_track_id.as_str(),
                    new_start_value,
                );
                if let Some(app) = weak.upgrade() {
                    let mut last = 0u64;
                    drain_events(&engine, &app, &projector, &mut last);
                }
            },
        );
    }

    let timeline = app.global::<TimelineLib>();
    timeline.on_sequence_duration(timeline::sequence_duration);
    timeline.on_format_timecode(|frame, fps_num, fps_den, drop_frame| {
        SharedString::from(crate::timecode::format_timecode(
            i64::from(frame),
            i64::from(fps_num),
            i64::from(fps_den),
            drop_frame,
        ))
    });

    app.global::<RulerBackend>().on_ticks(
        |scroll_x, viewport_w, zoom, fps_num, fps_den, drop_frame| {
            ruler::ticks_model(
                scroll_x,
                viewport_w,
                zoom,
                fps_num,
                fps_den,
                drop_frame,
            )
        },
    );

    app.global::<DragBackend>().on_snap_clip_start(
        |sequence,
         dragging_source_track_id,
         dragging_clip_id,
         cursor_start_value,
         clip_duration_ticks,
         snap_threshold_ticks| {
            let r = snap::compute_drag_snap(
                sequence,
                dragging_source_track_id.as_str(),
                dragging_clip_id.as_str(),
                cursor_start_value,
                clip_duration_ticks,
                snap_threshold_ticks,
            );
            SnapResult {
                has_snap: r.has_snap,
                snapped_start_value: r.snapped_start_value,
                snap_line_tick: r.snap_line_tick,
            }
        },
    );

    app.global::<DragBackend>().on_resolve_target_lane(
        |sequence, source_track_id, lane_offset| {
            let r = snap::resolve_drag_target(
                sequence,
                source_track_id.as_str(),
                lane_offset,
            );
            ResolvedTarget {
                track_id: r.track_id,
                clamped_offset: r.clamped_offset,
            }
        },
    );

    app.run()
}
