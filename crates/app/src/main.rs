//! Cutlass shell entry point.
//!
//! Boot sequence (default):
//!   1. Init tracing + WGPU-backed Slint backend.
//!   2. Build the dev [`demo`] project (probed `assets/` + starter timeline).
//!   3. Open [`EditorWindow`] with preview/import glue installed.
//!
//! Set `CUTLASS_LAUNCHER=1` to show the legacy launcher with "Create Project"
//! (empty project). `CUTLASS_ASSETS` overrides the demo media directory.

mod convert;
mod demo;
mod import;
mod preview;
mod session;
mod timeline_ui;

pub mod ui {
    //! Slint-generated types live here so they don't collide with the
    //! domain types from `models` (both expose `Project`, `Clip`, etc.).
    //! Outside this module use `ui::Project` for the DTO, `Project` for
    //! the domain.
    slint::include_modules!();
}

use std::cell::RefCell;
use std::rc::Rc;

use models::{
    Project, ProjectId, Rational, RationalTime, SchemaVersion, Sequence, SequenceId,
};
use slint::wgpu_28::WGPUConfiguration;
use slint::{BackendSelector, CloseRequestResponse, ComponentHandle};
use tracing::error;
use tracing_subscriber::EnvFilter;

use crate::preview::PreviewSession;
use crate::session::Session;
use crate::timeline_ui::Selection;
use crate::ui::{AppState, AppWindow, EditorWindow, TimelineState};

/// Canonical ticks-per-second for an empty sequence. 90 000 is the standard
/// MPEG timebase and divides evenly into 24/25/30/50/60 fps so quantizing
/// later is exact.
const DEFAULT_TIMEBASE: u32 = 90_000;

/// Owns everything that has to outlive a single editor session: the editor
/// window, the engine/preview drain thread, and the [`Session`] that owns
/// the authoritative `Project`.
///
/// Drop order matters — `Rc<RefCell<Session>>` is captured by Slint
/// callbacks held on `_window`, so the window must drop first to release
/// those references before the Session is dropped. Field declaration order
/// in this struct is the drop order Rust applies.
struct EditorSession {
    _window: EditorWindow,
    _preview: PreviewSession,
    _session: Rc<RefCell<Session>>,
}

fn setup_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    setup_tracing();
    BackendSelector::new()
        .require_wgpu_28(WGPUConfiguration::default())
        .select()?;

    let _session = if std::env::var_os("CUTLASS_LAUNCHER").is_some() {
        run_launcher()?;
        None
    } else {
        Some(open_editor(demo::project())?)
    };

    slint::run_event_loop()?;
    Ok(())
}

/// Legacy launcher: blank project on "Create Project".
fn run_launcher() -> Result<(), Box<dyn std::error::Error>> {
    let launcher = AppWindow::new()?;
    let session: Rc<RefCell<Option<EditorSession>>> = Rc::new(RefCell::new(None));
    let launcher_weak = launcher.as_weak();
    {
        let session = session.clone();
        launcher.on_create_project(move || match open_editor(empty_project()) {
            Ok(s) => {
                *session.borrow_mut() = Some(s);
                if let Some(l) = launcher_weak.upgrade() {
                    let _ = l.hide();
                }
            }
            Err(e) => error!(?e, "failed to open editor window"),
        });
    }
    launcher.show()?;
    Ok(())
}

/// Open the editor on `project`. Wires preview/import and quits the event loop
/// when the user closes the window.
///
/// Project ownership lives on [`Session`]; the Slint `AppState.project`
/// is a derived view refreshed by the `on_changed` callback installed at
/// construction. Every mutation path (timeline commands today, more
/// later) goes through the Session.
fn open_editor(project: Project) -> Result<EditorSession, Box<dyn std::error::Error>> {
    let editor = EditorWindow::new()?;

    // Ephemeral UI state (ruler fps) is one-shot at seed time. fps is
    // a sequence-level setting and there's no `Command` to change it
    // mid-edit, so we don't need to re-sync this on every project change.
    let fps = project.sequence.fps.as_f32().max(1.0);
    editor.global::<TimelineState>().set_fps(fps);

    let selection = timeline_ui::new_selection();
    let session = build_session(&editor, project, selection.clone());

    let preview = preview::install(&editor);
    import::install(&editor, session.clone());
    timeline_ui::install(&editor, session.clone(), selection);

    editor.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        CloseRequestResponse::HideWindow
    });

    editor.show()?;
    Ok(EditorSession {
        _window: editor,
        _preview: preview,
        _session: session,
    })
}

/// Build a `Session` whose `on_changed` callback rebuilds the Slint
/// `AppState.project` DTO and pushes it onto the editor window.
///
/// The callback runs synchronously inside `Session::submit` /
/// `Session::add_media`, so it MUST NOT re-enter the Session (e.g. by
/// reading from `AppState` and trying to mutate the project from another
/// callback). `set_project` is just a property write — Slint won't fire
/// further Rust callbacks synchronously from it.
fn build_session(
    editor: &EditorWindow,
    project: Project,
    selection: Selection,
) -> Rc<RefCell<Session>> {
    let weak = editor.as_weak();
    Rc::new(RefCell::new(Session::new(project, move |project| {
        if let Some(editor) = weak.upgrade() {
            let selected = *selection.borrow();
            let dto = convert::project_to_ui(project, selected);
            editor.global::<AppState>().set_project(dto);
        }
    })))
}

/// A blank project: 1920×1080 / 30 fps sequence, empty media bin and tracks.
pub(crate) fn empty_project() -> Project {
    let sequence = Sequence {
        id: SequenceId::new(),
        name: "Untitled Sequence".into(),
        width: 1920,
        height: 1080,
        fps: Rational::new_raw(30, 1),
        sample_rate: 48_000,
        timebase: DEFAULT_TIMEBASE,
        duration: RationalTime::new_raw(0, DEFAULT_TIMEBASE),
        in_point: None,
        out_point: None,
        tracks: vec![],
    };

    Project {
        id: ProjectId::new(),
        name: "Untitled".into(),
        file_path: None,
        schema: SchemaVersion::CURRENT,
        sequence,
        media_bin: Vec::new(),
        is_dirty: false,
    }
}
