//! Cutlass shell entry point.
//!
//! Boot sequence:
//!   1. Init tracing + WGPU-backed Slint backend.
//!   2. Show the launcher (`AppWindow`) with a "Create Project" button.
//!   3. On click, build a fresh empty `models::Project`, hand it to a new
//!      `EditorWindow`, install the engine/preview glue, then hide the
//!      launcher.
//!   4. Closing the editor quits the event loop.
//!
//! Once import / open-file land, the launcher will grow more entry points
//! (recent projects, "Open…", drag-drop) but the editor side stays the same.

mod convert;
mod import;
mod preview;

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
    Project, ProjectId, Rational, RationalTime, SchemaVersion, Sequence, SequenceId, Track,
    TrackId, TrackKind,
};
use slint::wgpu_28::WGPUConfiguration;
use slint::{BackendSelector, CloseRequestResponse, ComponentHandle};
use tracing::error;
use tracing_subscriber::EnvFilter;

use crate::preview::PreviewSession;
use crate::ui::{AppState, AppWindow, EditorWindow, TimelineState};

/// Canonical ticks-per-second for an empty sequence. 90 000 is the standard
/// MPEG timebase and divides evenly into 24/25/30/50/60 fps so quantizing
/// later is exact.
const DEFAULT_TIMEBASE: u32 = 90_000;

/// Owns everything that has to outlive a single `create-project` click:
/// the editor window itself plus the engine/preview drain thread.
struct EditorSession {
    _window: EditorWindow,
    _preview: PreviewSession,
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

    let launcher = AppWindow::new()?;

    // The session is stashed in a RefCell so the create-project closure can
    // own it for the lifetime of the event loop. Only one session can ever
    // be active at a time today.
    let session: Rc<RefCell<Option<EditorSession>>> = Rc::new(RefCell::new(None));
    let launcher_weak = launcher.as_weak();
    {
        let session = session.clone();
        launcher.on_create_project(move || match open_editor() {
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
    slint::run_event_loop()?;
    Ok(())
}

/// Build a fresh empty project and open the editor on it. Wires up the
/// preview engine and registers a close handler that quits the event loop
/// so the process exits when the user closes the editor.
fn open_editor() -> Result<EditorSession, Box<dyn std::error::Error>> {
    let editor = EditorWindow::new()?;
    seed_project(&editor, empty_project());
    let preview = preview::install(&editor);
    import::install(&editor);

    editor.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        CloseRequestResponse::HideWindow
    });

    editor.show()?;
    Ok(EditorSession {
        _window: editor,
        _preview: preview,
    })
}

/// Push a domain `Project` into the editor's Slint state. Mirrors the FPS
/// onto `TimelineState` so the ruler's frame-mode labelling stays correct.
fn seed_project(editor: &EditorWindow, project: Project) {
    let fps = project.sequence.fps.as_f32().max(1.0);
    editor.global::<TimelineState>().set_fps(fps);

    let dto: ui::Project = (&project).into();
    editor.global::<AppState>().set_project(dto);
}

/// A blank project: 1920×1080 / 30 fps sequence, default V1 + A1 tracks,
/// nothing in the media bin. Matches what a typical NLE drops you into on
/// "New Project" and gives the timeline two empty lanes to show structure.
fn empty_project() -> Project {
    let track_v1 = TrackId::new();
    let track_a1 = TrackId::new();

    let tracks = vec![];

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
        tracks,
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
