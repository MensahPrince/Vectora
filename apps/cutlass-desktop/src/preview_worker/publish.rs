use super::*;

/// Snapshot the engine's project and hand it to the UI thread, which rebuilds
/// the Slint view model. The snapshot crosses the thread boundary (`Send`);
/// the `!Send` Slint model types are constructed inside the event-loop closure.
/// History availability rides along so the toolbar's undo/redo states always
/// match the projection they were published with.
///
/// PORT (Phase 3): the audio mixer's timeline snapshot publishes from this
/// same chokepoint on main, so what playback sounds like can never diverge
/// from what the UI shows.
pub(super) fn publish_projection(engine: &mut Engine, ui: &UiSink) {
    let generator_sizes = generator_content_sizes(engine);
    // Pool entries whose backing file is gone (raw ids) — drives the relink
    // dialog count and the library tiles' missing badges. Computed here on
    // the worker thread so the UI thread never stats the filesystem (a dead
    // network mount must not hitch painting).
    let missing_media: std::collections::HashSet<u64> = engine
        .project()
        .media_iter()
        .filter(|m| !m.path().exists())
        .map(|m| m.id.raw())
        .collect();
    let project = engine.project().clone();
    // The audio mixer hears every edit through the same chokepoint that
    // republishes the view model, so sound and picture can't drift apart.
    ui.audio.publish_snapshot(project.clone());
    let can_undo = engine.can_undo();
    let can_redo = engine.can_redo();
    // Session save state rides the same chokepoint as the project view, so
    // the title bar's dirty dot can never disagree with the engine.
    let dirty = engine.is_dirty();
    let file_name = engine
        .project_path()
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let has_path = engine.project_path().is_some();
    // Full path (not just the stem): main.rs needs it to address the
    // session's autosave slot when a close discards unsaved work.
    let file_path = engine
        .project_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let editor_weak = ui.editor.clone();
    if let Err(e) = slint::invoke_from_event_loop(move || {
        if let Some(store) = editor_weak.upgrade() {
            store.set_project(crate::projection::project_to_slint(
                &project,
                &generator_sizes,
                &missing_media,
            ));
            store.set_missing_media_count(missing_media.len() as i32);
            store.set_can_undo(can_undo);
            store.set_can_redo(can_redo);
            store.set_projection_revision(store.get_projection_revision().saturating_add(1));
            store.set_project_dirty(dirty);
            store.set_project_has_path(has_path);
            store.set_project_file_name(file_name.into());
            store.set_project_file_path(file_path.into());
        }
    }) {
        error!("failed to publish project projection to UI: {e}");
    }
}

/// Bump `EditorStore.session-epoch`: the session was replaced wholesale
/// (open / new), and UI-side session state — playhead, selection, in/out
/// range, playback — must reset. The watcher lives in `app.slint`.
pub(super) fn bump_session_epoch(ui: &UiSink) {
    let editor_weak = ui.editor.clone();
    if let Err(e) = slint::invoke_from_event_loop(move || {
        if let Some(store) = editor_weak.upgrade() {
            store.set_session_epoch(store.get_session_epoch() + 1);
        }
    }) {
        error!("failed to bump session epoch: {e}");
    }
}

/// Surface a session-level failure (save/open) to the user: sets
/// `EditorStore.session-error`, which mounts the message dialog until the
/// user dismisses it (clearing the property).
pub(super) fn publish_session_error(ui: &UiSink, message: String) {
    let editor_weak = ui.editor.clone();
    if let Err(e) = slint::invoke_from_event_loop(move || {
        if let Some(store) = editor_weak.upgrade() {
            store.set_session_error(message.into());
        }
    }) {
        error!("failed to publish session error: {e}");
    }
}

/// Drawn-content size (canvas px) for every generated clip, keyed by raw clip
/// id — the preview's selection box and hit-test hug what the generator
/// actually draws instead of its full-canvas raster. Served from the engine's
/// raster caches; clips the compositor doesn't draw are absent (the UI falls
/// back to canvas size). Animated params are sampled at the clip's first
/// frame — the projection republishes per edit, not per playhead move, so a
/// single representative size is all it can carry.
pub(super) fn generator_content_sizes(engine: &mut Engine) -> HashMap<u64, (i32, i32)> {
    let generators: Vec<(u64, Generator)> = engine
        .project()
        .timeline()
        .tracks_ordered()
        .flat_map(|track| track.clips())
        .filter_map(|clip| match &clip.content {
            ClipSource::Generated(generator) => Some((clip.id.raw(), generator.clone())),
            ClipSource::Media { .. } => None,
        })
        .collect();
    generators
        .into_iter()
        .filter_map(|(id, generator)| {
            let (w, h) = engine.generator_content_size(&generator, 0)?;
            Some((id, (w as i32, h as i32)))
        })
        .collect()
}

// PORT (Phase 3): main built the playback mixer's `AudioSnapshot` here (every
// audible clip with its source window, retime, volume envelope, and fades).
// It returns with the cpal audio system on `ExportAudioMixer`.
