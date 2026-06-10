mod inspector;
mod preview;
mod preview_worker;
mod projection;
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
    let app_weak = app.as_weak();
    slint::invoke_from_event_loop(move || {
        if let Some(app) = app_weak.upgrade() {
            app.window().set_maximized(true);
        }
    })
    .map_err(|e| slint::PlatformError::from(format!("failed to schedule maximize: {e}")))?;
    let preview_store_weak = app.global::<PreviewStore>().as_weak();
    let editor_store_weak = app.global::<EditorStore>().as_weak();

    let (preview_worker, session) = preview_worker::PreviewWorker::spawn(
        EngineConfig::default(),
        preview_store_weak,
        editor_store_weak,
    )
    .map_err(slint::PlatformError::from)?;

    info!(
        duration_ticks = session.duration_ticks,
        tl_rate = ?session.tl_rate,
        "preview worker ready; import media to populate the timeline"
    );

    let duration_ticks = session.duration_ticks;
    let editor = app.global::<EditorStore>();

    let frame_handle = preview_worker.handle();
    editor.on_on_slider_changed(move |value| {
        frame_handle.request_frame(slider_to_timeline_tick(value, duration_ticks));
    });

    let drop_handle = preview_worker.handle();
    editor.on_on_clip_dropped(move |media_id, track_id, start_tick, drop_row| {
        drop_handle.add_clip(
            media_id.to_string(),
            track_id.to_string(),
            i64::from(start_tick),
            i64::from(drop_row),
        );
    });

    let import_handle = preview_worker.handle();
    editor.on_on_import_clicked(move || {
        // Native picker is modal and must run on the main thread — which is
        // exactly where this Slint callback fires. The engine work happens off
        // this thread once we hand the path to the worker.
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Video", &["mp4", "mov", "mkv", "webm", "m4v"])
            .pick_file()
        {
            import_handle.import(path);
        }
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

    let editor_weak = app.global::<EditorStore>().as_weak();
    app.global::<InspectorBackend>()
        .on_resolve_selection(|sequence, track_id, clip_id| {
            inspector::resolve_selection(sequence, track_id.as_str(), clip_id.as_str())
        });
    app.global::<InspectorBackend>()
        .on_set_text_content(move |track_id, clip_id, content| {
            let Some(editor) = editor_weak.upgrade() else {
                return;
            };
            let mut project = editor.get_project();
            inspector::set_text_content(
                &mut project,
                track_id.as_str(),
                clip_id.as_str(),
                content.as_str(),
            );
            editor.set_project(project);
        });

    app.run()
}
