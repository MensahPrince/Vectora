#![allow(unused_imports)]

use std::cell::Cell;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use cutlass_engine::EngineConfig;
use slint::ComponentHandle;
use slint::Global;
use slint::Model;
use slint::ModelRc;
use slint::SharedString;
use slint::VecModel;

// --- session lifecycle: app-owned drafts, auto-saved (CapCut-style) -------
//
// Cutlass owns every project as a draft under the per-user data dir (see the
// `drafts` module); the worker auto-saves the live draft after every edit, so
// the user never saves by hand and a clean exit loses nothing. Switching the
// live session — New, opening another draft, importing a file — flushes the
// outgoing draft first (an ordered `SaveProject` on the worker's queue), then
// swaps. Closing is handled separately (`request_close`): the editor returns
// to the launch gallery, the gallery quits the app.

pub(crate) enum SessionChange {
    /// Create a fresh, empty draft and switch to it.
    New,
    /// Pick an external `.cutlass` file and import it into a new draft.
    Import,
    /// Open an existing draft by its `project.cutlass` path (gallery card).
    OpenDraft(std::path::PathBuf),
}

/// Flush the outgoing draft (a no-op when nothing is bound yet), then replace
/// the session. The flush and the swap are ordered on the worker's single
/// message queue, so the flush always captures the draft we're leaving.
pub(crate) fn protect_draft_downloads(
    cache: &cutlass_cloud::cache::DownloadCache,
    project_path: &std::path::Path,
) {
    let Ok(project) = cutlass_models::Project::load_from_file(project_path) else {
        cache.block_destructive_operations();
        tracing::warn!(
            "download cache maintenance blocked: target project could not be inventoried"
        );
        return;
    };
    let counts = crate::download_safety::protect_project_downloads(cache, &project);
    if counts.rejected != 0 {
        cache.block_destructive_operations();
        tracing::warn!(
            rejected = counts.rejected,
            "download cache maintenance blocked: target project protection was incomplete"
        );
    }
}

pub(crate) fn change_session(
    handle: &crate::preview_worker::WorkerHandle,
    download_cache: &Arc<cutlass_cloud::cache::DownloadCache>,
    change: SessionChange,
) {
    match change {
        SessionChange::New => match crate::drafts::create() {
            Ok(path) => {
                handle.save_project(None);
                handle.new_project();
                handle.save_project(Some(path));
            }
            Err(e) => tracing::error!("couldn't create a new project: {e}"),
        },
        SessionChange::OpenDraft(path) => {
            protect_draft_downloads(download_cache, &path);
            handle.save_project(None);
            handle.open_project(path);
        }
        SessionChange::Import => {
            let handle = handle.clone();
            let download_cache = Arc::clone(download_cache);
            let task = slint::spawn_local(async move {
                if let Some(source) = crate::library_helpers::pick_open_path().await {
                    match crate::drafts::import_external(&source) {
                        Ok(path) => {
                            protect_draft_downloads(&download_cache, &path);
                            handle.save_project(None);
                            handle.open_project(path);
                        }
                        Err(e) => {
                            tracing::error!("couldn't import {}: {e}", source.display())
                        }
                    }
                }
            });
            if let Err(e) = task {
                tracing::error!("failed to open import dialog: {e}");
            }
        }
    }
}

/// Republish the launch gallery from the draft store, newest first.
pub(crate) fn refresh_projects(app: &crate::AppWindow) {
    let rows: Vec<crate::ProjectSummary> = crate::drafts::list()
        .into_iter()
        .map(|draft| crate::ProjectSummary {
            name: draft.name.into(),
            path: draft.project.to_string_lossy().into_owned().into(),
            modified: crate::drafts::relative_time(draft.modified).into(),
        })
        .collect();
    app.global::<crate::EditorStore>()
        .set_projects(ModelRc::new(VecModel::from(rows)));
}

/// The window close button, context-aware (CapCut-style). In the editor it
/// flushes the draft and returns to the launch gallery (refreshed) — the work
/// is already auto-saved, so there's no prompt and the app stays open; on the
/// gallery there's nothing left to return to, so it quits. Wired to both the
/// custom caption ✕ and the OS close request (the macOS traffic light).
pub(crate) fn request_close(
    app_weak: &slint::Weak<crate::AppWindow>,
    handle: &crate::preview_worker::WorkerHandle,
) {
    let Some(app) = app_weak.upgrade() else {
        return;
    };
    if app.global::<crate::AppState>().get_launch_visible() {
        let _ = slint::quit_event_loop();
    } else {
        handle.save_project(None);
        refresh_projects(&app);
        app.global::<crate::AppState>().set_launch_visible(true);
    }
}

/// A trimmed, non-empty string, else `None` — the shape `cutlass_settings`'
/// optional fields want (an empty text box clears the key rather than writing
/// `""`).
pub(crate) fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}
