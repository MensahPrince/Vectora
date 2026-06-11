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

    let editor = app.global::<EditorStore>();

    // Playhead moves (ruler scrub, frame-step keys, Home/End) become preview
    // frame requests; the worker coalesces a burst to the newest tick.
    let frame_handle = preview_worker.handle();
    editor.on_on_playhead_changed(move |tick| {
        frame_handle.request_frame(i64::from(tick));
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

    app.global::<RulerBackend>().on_ticks(|scroll_x, viewport_w, zoom, fps_num, fps_den| {
        ruler::ticks_model(scroll_x, viewport_w, zoom, fps_num, fps_den)
    });

    app.global::<DragBackend>().on_snap_clip_start(
        |sequence,
         dragging_source_track_id,
         dragging_clip_id,
         cursor_start_value,
         clip_duration_ticks,
         snap_threshold_ticks,
         playhead_tick| {
            snap::compute_drag_snap(
                &sequence,
                dragging_source_track_id.as_str(),
                dragging_clip_id.as_str(),
                cursor_start_value,
                clip_duration_ticks,
                snap_threshold_ticks,
                playhead_tick,
            )
        },
    );

    app.global::<DragBackend>().on_resolve_clip_drag(
        |sequence, source_track_id, dragging_clip_id, dx_ticks, hover_row, playhead_tick, snap_threshold_ticks| {
            snap::resolve_clip_drag(
                &sequence,
                source_track_id.as_str(),
                dragging_clip_id.as_str(),
                dx_ticks,
                hover_row,
                playhead_tick,
                snap_threshold_ticks,
            )
        },
    );

    app.global::<DragBackend>().on_resolve_clip_trim(
        |sequence, track_id, clip_id, trim_head, dx_ticks, playhead_tick, snap_threshold_ticks| {
            snap::resolve_clip_trim(
                &sequence,
                track_id.as_str(),
                clip_id.as_str(),
                trim_head,
                dx_ticks,
                playhead_tick,
                snap_threshold_ticks,
            )
        },
    );

    let move_handle = preview_worker.handle();
    editor.on_on_clip_moved(move |clip_id, track_id, insert_row, start_tick| {
        move_handle.move_clip(
            clip_id.to_string(),
            track_id.to_string(),
            i64::from(insert_row),
            i64::from(start_tick),
        );
    });

    let trim_handle = preview_worker.handle();
    editor.on_on_clip_trimmed(move |clip_id, start_tick, duration_ticks| {
        trim_handle.trim_clip(
            clip_id.to_string(),
            i64::from(start_tick),
            i64::from(duration_ticks),
        );
    });

    // --- Phase 5: selection ops & history (UI gates, engine validates) ---

    let delete_handle = preview_worker.handle();
    editor.on_on_clip_deleted(move |clip_id| {
        delete_handle.remove_clip(clip_id.to_string());
    });

    let split_handle = preview_worker.handle();
    editor.on_on_clip_split(move |clip_id, at_tick| {
        split_handle.split_clip(clip_id.to_string(), i64::from(at_tick));
    });

    let undo_handle = preview_worker.handle();
    editor.on_on_undo(move || {
        undo_handle.undo();
    });

    let redo_handle = preview_worker.handle();
    editor.on_on_redo(move || {
        redo_handle.redo();
    });

    let copy_handle = preview_worker.handle();
    editor.on_on_clip_copied(move |clip_id| {
        copy_handle.copy_clip(clip_id.to_string());
    });

    let paste_handle = preview_worker.handle();
    editor.on_on_paste_at(move |tick| {
        paste_handle.paste_at(i64::from(tick));
    });

    let duplicate_handle = preview_worker.handle();
    editor.on_on_clip_duplicated(move |clip_id| {
        duplicate_handle.duplicate_clip(clip_id.to_string());
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
