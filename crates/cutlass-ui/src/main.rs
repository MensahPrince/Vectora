mod preview;
mod preview_worker;
mod ruler;
mod snap;
mod timecode;
mod timeline;

use slint::BackendSelector;
use slint::Global;
use slint::SharedString;
use slint::wgpu_28::WGPUConfiguration;
use tracing::info;
use tracing_subscriber::EnvFilter;

use cutlass_engine::EngineConfig;
use std::path::PathBuf;

slint::include_modules!();

fn setup_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn slider_to_timeline_tick(value: f32, duration_ticks: i64) -> i64 {
    if duration_ticks <= 0 {
        return 0;
    }
    let max_tick = duration_ticks - 1;
    ((value.clamp(0.0, 100.0) / 100.0) * max_tick as f32).round() as i64
}

fn main() -> Result<(), slint::PlatformError> {
    setup_tracing();
    BackendSelector::new()
        .require_wgpu_28(WGPUConfiguration::default())
        .select()?;

    let app = AppWindow::new()?;
    let preview_store_weak = app.global::<PreviewStore>().as_weak();

    let (preview_worker, session) = preview_worker::PreviewWorker::spawn(
        EngineConfig::default(),
        PathBuf::from("assets/16078866_3840_2160_60fps.mp4"),
        preview_store_weak,
    )
    .map_err(slint::PlatformError::from)?;

    info!(
        duration_ticks = session.duration_ticks,
        tl_rate = ?session.tl_rate,
        "timeline ready for scrub"
    );

    preview_worker.request_frame(0);

    let duration_ticks = session.duration_ticks;
    let editor = app.global::<EditorStore>();
    editor.on_on_slider_changed(move |value| {
        preview_worker.request_frame(slider_to_timeline_tick(value, duration_ticks));
    });

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
            ruler::ticks_model(scroll_x, viewport_w, zoom, fps_num, fps_den, drop_frame)
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

    app.global::<DragBackend>()
        .on_resolve_target_lane(|sequence, source_track_id, lane_offset| {
            let r = snap::resolve_drag_target(sequence, source_track_id.as_str(), lane_offset);
            ResolvedTarget {
                track_id: r.track_id,
                clamped_offset: r.clamped_offset,
            }
        });

    app.run()
}
