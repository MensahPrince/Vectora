use super::*;

pub(super) const UI_WAIT_SLICE: Duration = Duration::from_millis(25);
pub(super) const UI_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) type UiResponse = Result<Value, String>;

/// Parse and execute one registered app-control tool.
///
/// This function is called from the agent thread. It never upgrades the Slint
/// weak handle or reads a generated global until the event-loop closure runs.
pub fn call(
    app: slint::Weak<crate::AppWindow>,
    name: &str,
    arguments: &Value,
    cancel: &AtomicBool,
) -> Result<ToolOutput, String> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled before the app-control tool could run".into());
    }

    let request = parse_request(name, arguments)?;

    // Theme IO deliberately precedes event-loop dispatch. A malformed config
    // fails closed, and cutlass-settings' format-preserving save protects
    // comments and unknown/newer keys. Once the save succeeds, finish the
    // matching live update even if cancellation arrives between the two
    // operations, so persisted and visible state cannot diverge.
    if let Request::Theme(theme) = &request {
        persist_theme_at(&cutlass_settings::default_config_path(), *theme)?;
        let finish_live_update = AtomicBool::new(false);
        return dispatch_ui(app, &finish_live_update, move |app| {
            execute_ui(&app, request)
        });
    }

    dispatch_ui(app, cancel, move |app| execute_ui(&app, request))
}

pub(super) fn dispatch_ui(
    app: slint::Weak<crate::AppWindow>,
    cancel: &AtomicBool,
    operation: impl FnOnce(crate::AppWindow) -> UiResponse + Send + 'static,
) -> Result<ToolOutput, String> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled before the app-control UI dispatch".into());
    }

    let (response_tx, response_rx) = bounded::<UiResponse>(1);
    let abandoned = Arc::new(AtomicBool::new(false));
    let event_loop_abandoned = Arc::clone(&abandoned);

    if let Err(error) = slint::invoke_from_event_loop(move || {
        if event_loop_abandoned.load(Ordering::Acquire) {
            return;
        }
        let response = app
            .upgrade()
            .ok_or_else(|| "the Cutlass window is closed".to_string())
            .and_then(operation);
        if !event_loop_abandoned.load(Ordering::Acquire) {
            let _ = response_tx.try_send(response);
        }
    }) {
        abandoned.store(true, Ordering::Release);
        tracing::debug!(%error, "agent app-control event-loop dispatch failed");
        return Err("the Cutlass event loop is unavailable; the app may be closing".to_string());
    }

    let value = wait_for_ui_response(
        &response_rx,
        cancel,
        &abandoned,
        UI_WAIT_TIMEOUT,
        UI_WAIT_SLICE,
    )??;
    serde_json::to_string(&value)
        .map(ToolOutput::text)
        .map_err(|error| format!("could not encode app-control response: {error}"))
}

pub(super) fn wait_for_ui_response(
    receiver: &Receiver<UiResponse>,
    cancel: &AtomicBool,
    abandoned: &AtomicBool,
    timeout: Duration,
    wait_slice: Duration,
) -> Result<UiResponse, String> {
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Acquire) {
            abandoned.store(true, Ordering::Release);
            return Err("cancelled while waiting for the Cutlass UI".into());
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            abandoned.store(true, Ordering::Release);
            return Err(
                "the Cutlass UI did not respond within 2 seconds; it may be busy or closing".into(),
            );
        }

        match receiver.recv_timeout(wait_slice.min(remaining)) {
            Ok(response) => {
                if cancel.load(Ordering::Acquire) {
                    abandoned.store(true, Ordering::Release);
                    return Err("cancelled while waiting for the Cutlass UI".into());
                }
                return Ok(response);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                abandoned.store(true, Ordering::Release);
                return Err(
                    "the Cutlass UI response channel closed before the action completed".into(),
                );
            }
        }
    }
}

pub(super) fn execute_ui(app: &crate::AppWindow, request: Request) -> UiResponse {
    if request == Request::State {
        return Ok(app_state_value(app));
    }
    let result = match request {
        Request::State => unreachable!("state returned above"),
        Request::Playback(action) => execute_playback(app, action),
        Request::Seek(seconds) => execute_seek(app, seconds),
        Request::LoopRange(action) => execute_loop_range(app, action),
        Request::TimelineZoom(action) => execute_timeline_zoom(app, action),
        Request::SelectClip(action) => execute_selection(app, action),
        Request::Panel { panel, action } => Ok(execute_panel(app, panel, action)),
        Request::Theme(theme) => {
            app.global::<crate::AppStore>().set_theme_id(theme.index());
            Ok(json!({
                "theme": theme.key(),
                "status": "applied-and-saved"
            }))
        }
        Request::Window(action) => Ok(execute_window(app, action)),
        Request::WindowMove { x, y } => {
            app.window()
                .set_position(slint::PhysicalPosition::new(x, y));
            Ok(json!({
                "status": "requested",
                "position": { "x": x, "y": y }
            }))
        }
        Request::WindowResize { width, height } => {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            Ok(json!({
                "status": "requested",
                "size": { "width": width, "height": height }
            }))
        }
        Request::Close => {
            app.global::<crate::WindowBackend>().invoke_close();
            Ok(json!({ "status": "requested", "action": "close" }))
        }
    }?;
    Ok(state_echo(result, app_state_value(app)))
}

pub(super) fn state_echo(result: Value, state: Value) -> Value {
    json!({ "result": result, "state": state })
}

pub(super) fn app_state_value(app: &crate::AppWindow) -> Value {
    let timeline = app.global::<crate::TimelineStore>();
    let editor = app.global::<crate::EditorStore>();
    let state = app.global::<crate::AppState>();
    let app_store = app.global::<crate::AppStore>();
    let view = app.global::<crate::TimelineViewState>();
    let project = editor.get_project();
    let playhead_ticks = timeline.get_playhead_tick();
    let playhead_seconds = ticks_to_seconds(playhead_ticks, &project.sequence.fps);
    let duration_ticks = sequence_end_tick(app, project.sequence.clone());
    let duration_seconds = ticks_to_seconds(duration_ticks, &project.sequence.fps);
    let range_in_tick = timeline.get_range_in_tick();
    let range_out_tick = timeline.get_range_out_tick();
    let range_in_seconds =
        (range_in_tick >= 0).then(|| ticks_to_seconds(range_in_tick, &project.sequence.fps));
    let range_out_seconds =
        (range_out_tick >= 0).then(|| ticks_to_seconds(range_out_tick, &project.sequence.fps));
    let selected_ids = model_strings(&timeline.get_selected_ids());
    let position = app.window().position();
    let size = app.window().size();

    json!({
        "transport": {
            "playing": timeline.get_playing(),
            "playhead_tick": playhead_ticks,
            "playhead_seconds": playhead_seconds,
            "speed_num": timeline.get_play_speed_num(),
            "speed_den": timeline.get_play_speed_den(),
            "loop_enabled": timeline.get_loop_enabled(),
            "range_in_tick": range_in_tick,
            "range_out_tick": range_out_tick,
            "range_in_seconds": range_in_seconds,
            "range_out_seconds": range_out_seconds
        },
        "timeline": {
            "zoom": timeline.get_zoom(),
            "scroll_x": view.get_scroll_x(),
            "scroll_y": view.get_scroll_y(),
            "selected_track_id": timeline.get_selected_track_id().as_str(),
            "selected_clip_id": timeline.get_selected_clip_id().as_str(),
            "selected_clip_ids": selected_ids
        },
        "sequence": {
            "fps_num": project.sequence.fps.num,
            "fps_den": project.sequence.fps.den,
            "duration_ticks": duration_ticks,
            "duration_seconds": duration_seconds
        },
        "theme": theme_key_from_index(app_store.get_theme_id()),
        "panels": {
            "library": state.get_library_panel_open(),
            "inspector": state.get_inspector_panel_open(),
            "assistant": state.get_agent_panel_open(),
            "timeline": state.get_timeline_panel_open(),
            "preview": state.get_preview_maximized(),
            "library_width": app.get_left_pane_width(),
            "inspector_width": app.get_right_pane_width(),
            "assistant_width": app.get_agent_pane_width(),
            "timeline_height": app.get_timeline_height()
        },
        "dialogs": {
            "settings": state.get_settings_open(),
            "export": state.get_export_dialog_open(),
            "canvas": state.get_canvas_dialog_open(),
            "relink": state.get_relink_dialog_open(),
            "confirm_delete_media": state.get_confirm_delete_media_open()
        },
        "launch_visible": state.get_launch_visible(),
        "window": {
            "position": { "x": position.x, "y": position.y },
            "size": { "width": size.width, "height": size.height },
            "minimized": app.window().is_minimized(),
            "maximized": app.window().is_maximized(),
            "fullscreen": app.window().is_fullscreen()
        }
    })
}

pub(super) fn execute_playback(app: &crate::AppWindow, action: PlaybackAction) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let timeline = app.global::<crate::TimelineStore>();
    let actions = app.global::<crate::TimelineActions>();
    let was_playing = timeline.get_playing();

    match action {
        PlaybackAction::Play if !was_playing => actions.invoke_play(),
        PlaybackAction::Pause if was_playing => actions.invoke_pause(),
        PlaybackAction::Toggle => actions.invoke_toggle_play(),
        PlaybackAction::Stop => {
            actions.invoke_pause();
            actions.invoke_seek_start();
        }
        PlaybackAction::Play | PlaybackAction::Pause => {}
    }

    Ok(json!({
        "action": action.key(),
        "playing": timeline.get_playing(),
        "playhead_tick": timeline.get_playhead_tick()
    }))
}

pub(super) fn execute_seek(app: &crate::AppWindow, seconds: f64) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let editor = app.global::<crate::EditorStore>();
    let project = editor.get_project();
    let fps = project.sequence.fps.clone();
    if fps.num <= 0 || fps.den <= 0 {
        return Err("cannot seek because the timeline frame rate is invalid".into());
    }

    let requested = seconds_to_ticks(seconds, &fps);
    let clamped = app
        .global::<crate::TimelineActions>()
        .invoke_clamp_playhead(requested);
    let timeline = app.global::<crate::TimelineStore>();
    timeline.set_playhead_tick(clamped);

    Ok(json!({
        "playhead_tick": clamped,
        "playhead_seconds": ticks_to_seconds(clamped, &fps)
    }))
}

pub(super) fn execute_loop_range(app: &crate::AppWindow, action: LoopRangeAction) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let timeline = app.global::<crate::TimelineStore>();
    if let LoopRangeAction::Set {
        start_seconds,
        end_seconds,
    } = action
    {
        let sequence = app.global::<crate::EditorStore>().get_project().sequence;
        let fps = sequence.fps.clone();
        if fps.num <= 0 || fps.den <= 0 {
            return Err(
                "cannot set a loop range because the timeline frame rate is invalid".into(),
            );
        }
        let duration_tick = sequence_end_tick(app, sequence);
        let (start_tick, end_tick) =
            loop_range_ticks(start_seconds, end_seconds, &fps, duration_tick)?;
        timeline.set_range_in_tick(start_tick);
        timeline.set_range_out_tick(end_tick);
        timeline.set_loop_enabled(true);
    } else {
        match action {
            LoopRangeAction::Enable => {
                if timeline.get_range_in_tick() < 0
                    || timeline.get_range_out_tick() <= timeline.get_range_in_tick()
                {
                    return Err("cannot enable looping until a valid range is set".into());
                }
                timeline.set_loop_enabled(true);
            }
            LoopRangeAction::Disable => timeline.set_loop_enabled(false),
            LoopRangeAction::Clear => {
                timeline.set_loop_enabled(false);
                timeline.set_range_in_tick(-1);
                timeline.set_range_out_tick(-1);
            }
            LoopRangeAction::Set { .. } => unreachable!("set handled above"),
        }
    }

    Ok(json!({
        "action": action.key(),
        "loop_enabled": timeline.get_loop_enabled(),
        "range_in_tick": timeline.get_range_in_tick(),
        "range_out_tick": timeline.get_range_out_tick()
    }))
}

pub(super) fn execute_timeline_zoom(app: &crate::AppWindow, action: ZoomAction) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let actions = app.global::<crate::TimelineActions>();
    match action {
        ZoomAction::Set(value) => actions.invoke_set_zoom(value),
        ZoomAction::In => actions.invoke_zoom_in(),
        ZoomAction::Out => actions.invoke_zoom_out(),
        ZoomAction::Fit => actions.invoke_zoom_to_fit(),
    }
    Ok(json!({
        "action": action.key(),
        "zoom": app.global::<crate::TimelineStore>().get_zoom()
    }))
}

pub(super) fn execute_selection(app: &crate::AppWindow, action: SelectionAction) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let timeline = app.global::<crate::TimelineStore>();
    let (clip_id, toggle) = match action {
        SelectionAction::Select(clip_id) => (clip_id, false),
        SelectionAction::Toggle(clip_id) => (clip_id, true),
        SelectionAction::Clear => {
            timeline.set_selected_track_id(SharedString::new());
            timeline.set_selected_clip_id(SharedString::new());
            timeline.set_selected_ids(Default::default());
            return Ok(selection_value(&timeline));
        }
    };
    let sequence = app.global::<crate::EditorStore>().get_project().sequence;
    let (track_id, locked) = find_clip_track(&sequence, &clip_id)
        .ok_or_else(|| format!("unknown timeline clip id '{clip_id}'"))?;
    if locked {
        return Err(format!(
            "clip '{clip_id}' is on locked track '{track_id}'; unlock the track first"
        ));
    }

    let update = if toggle {
        crate::selection::toggle_clip(
            &sequence,
            &timeline.get_selected_ids(),
            &track_id,
            &clip_id,
            timeline.get_link_enabled(),
        )
    } else {
        crate::selection::select_clip(&sequence, &track_id, &clip_id, timeline.get_link_enabled())
    };
    timeline.invoke_apply_selection(update);
    Ok(selection_value(&timeline))
}

pub(super) fn selection_value(timeline: &crate::TimelineStore<'_>) -> Value {
    json!({
        "selected_track_id": timeline.get_selected_track_id().as_str(),
        "selected_clip_id": timeline.get_selected_clip_id().as_str(),
        "selected_clip_ids": model_strings(&timeline.get_selected_ids())
    })
}

pub(super) fn find_clip_track(sequence: &crate::Sequence, clip_id: &str) -> Option<(String, bool)> {
    for track_row in 0..sequence.tracks.row_count() {
        let Some(track) = sequence.tracks.row_data(track_row) else {
            continue;
        };
        for clip_row in 0..track.clips.row_count() {
            if track
                .clips
                .row_data(clip_row)
                .is_some_and(|clip| clip.id == clip_id)
            {
                return Some((track.id.to_string(), track.locked));
            }
        }
    }
    None
}

pub(super) fn execute_panel(
    app: &crate::AppWindow,
    panel: Panel,
    action: VisibilityAction,
) -> Value {
    let state = app.global::<crate::AppState>();
    let current = match panel {
        Panel::Library => state.get_library_panel_open(),
        Panel::Inspector => state.get_inspector_panel_open(),
        Panel::Assistant => state.get_agent_panel_open(),
        Panel::Timeline => state.get_timeline_panel_open(),
        Panel::Preview => state.get_preview_maximized(),
        Panel::Settings => state.get_settings_open(),
        Panel::Export => state.get_export_dialog_open(),
        Panel::Canvas => state.get_canvas_dialog_open(),
    };
    let visible = action.apply(current);
    match panel {
        Panel::Library => state.set_library_panel_open(visible),
        Panel::Inspector => state.set_inspector_panel_open(visible),
        Panel::Assistant => state.set_agent_panel_open(visible),
        Panel::Timeline => state.set_timeline_panel_open(visible),
        Panel::Preview => state.set_preview_maximized(visible),
        Panel::Settings => state.set_settings_open(visible),
        Panel::Export => state.set_export_dialog_open(visible),
        Panel::Canvas => state.set_canvas_dialog_open(visible),
    }
    json!({ "panel": panel.key(), "visible": visible })
}

pub(super) fn execute_window(app: &crate::AppWindow, action: WindowAction) -> Value {
    match action {
        WindowAction::Minimize => app.global::<crate::WindowBackend>().invoke_minimize(),
        WindowAction::Restore => {
            app.window().set_minimized(false);
            app.global::<crate::AppState>().set_preview_maximized(false);
            app.global::<crate::WindowBackend>()
                .invoke_set_maximized(false);
        }
        WindowAction::Maximize => app
            .global::<crate::WindowBackend>()
            .invoke_set_maximized(true),
        // AppWindow's full-screen property is bound to this existing state;
        // changing the state preserves that binding and the preview layout.
        WindowAction::Fullscreen => app.global::<crate::AppState>().set_preview_maximized(true),
        WindowAction::ExitFullscreen => {
            app.global::<crate::AppState>().set_preview_maximized(false)
        }
    }
    json!({ "status": "requested", "action": action.key() })
}

pub(super) fn ensure_timeline_control_available(app: &crate::AppWindow) -> Result<(), String> {
    let state = app.global::<crate::AppState>();
    if state.get_launch_visible() {
        return Err(
            "timeline control is unavailable on the launch screen; open a project first".into(),
        );
    }
    if state.get_settings_open()
        || state.get_export_dialog_open()
        || state.get_canvas_dialog_open()
        || state.get_relink_dialog_open()
        || state.get_confirm_delete_media_open()
        || !app
            .global::<crate::EditorStore>()
            .get_session_error()
            .is_empty()
    {
        return Err(
            "timeline control is blocked by an open dialog; close the dialog and retry".into(),
        );
    }
    Ok(())
}

pub(super) fn sequence_end_tick(app: &crate::AppWindow, sequence: crate::Sequence) -> i32 {
    app.global::<crate::TimelineLib>()
        .invoke_sequence_duration(sequence)
        .value
        .max(0)
}

pub(super) fn seconds_to_ticks(seconds: f64, fps: &crate::Rational) -> i32 {
    let ticks = seconds * f64::from(fps.num) / f64::from(fps.den);
    if ticks >= f64::from(i32::MAX) {
        i32::MAX
    } else {
        ticks.round().max(0.0) as i32
    }
}

pub(super) fn ticks_to_seconds(ticks: i32, fps: &crate::Rational) -> f64 {
    if fps.num <= 0 || fps.den <= 0 {
        return 0.0;
    }
    f64::from(ticks) * f64::from(fps.den) / f64::from(fps.num)
}

pub(super) fn loop_range_ticks(
    start_seconds: f64,
    end_seconds: f64,
    fps: &crate::Rational,
    duration_tick: i32,
) -> Result<(i32, i32), String> {
    let start_tick = seconds_to_ticks(start_seconds, fps);
    let end_tick = seconds_to_ticks(end_seconds, fps);
    if start_tick >= end_tick {
        return Err("loop range endpoints collapse to the same frame; choose a wider range".into());
    }
    if end_tick > duration_tick {
        return Err(format!(
            "loop range ends at {end_seconds:.3}s, beyond the {:.3}s sequence",
            ticks_to_seconds(duration_tick, fps)
        ));
    }
    Ok((start_tick, end_tick))
}

pub(super) fn model_strings(model: &slint::ModelRc<SharedString>) -> Vec<String> {
    (0..model.row_count())
        .filter_map(|row| model.row_data(row))
        .map(|value| value.to_string())
        .collect()
}
