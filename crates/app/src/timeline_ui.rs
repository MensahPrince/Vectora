//! Timeline editing gestures: clip selection and move.
//!
//! Slint fires [`TimelineInteraction`] callbacks; this module translates
//! them into ephemeral UI state (selection) or [`timeline::Command`]s
//! (move) routed through [`Session::submit`].

use std::cell::RefCell;
use std::rc::Rc;
use std::str::FromStr;

use models::{ClipId, RationalTime};
use slint::ComponentHandle;
use timeline::{Command, MoveClip};
use tracing::warn;

use crate::convert::project_to_ui;
use crate::session::Session;
use crate::ui::{AppState, EditorWindow, TimelineInteraction};

/// Shared selection handle. Held beside the session so `on_changed` can
/// stamp `Clip.selected` on the DTO without persisting selection in the domain.
pub type Selection = Rc<RefCell<Option<ClipId>>>;

pub fn new_selection() -> Selection {
    Rc::new(RefCell::new(None))
}

pub fn install(editor: &EditorWindow, session: Rc<RefCell<Session>>, selection: Selection) {
    let weak = editor.as_weak();
    let session_select = session.clone();
    let selection_select = selection.clone();

    editor
        .global::<TimelineInteraction>()
        .on_select_clip(move |clip_id| {
            let parsed = parse_clip_id(&clip_id);
            *selection_select.borrow_mut() = parsed;
            if let Some(editor) = weak.upgrade() {
                publish_project(&editor, &session_select, &selection_select);
                editor
                    .global::<TimelineInteraction>()
                    .set_selected_clip_id(clip_id);
            }
        });

    let weak_move = editor.as_weak();
    let session_move = session.clone();
    let selection_move = selection.clone();

    editor
        .global::<TimelineInteraction>()
        .on_move_clip(move |clip_id, new_start_sec| {
            let Some(clip_id) = parse_clip_id(&clip_id) else {
                return;
            };
            let Ok(new_start) = sec_to_timeline_time(&session_move, new_start_sec) else {
                return;
            };
            let cmd = Command::MoveClip(MoveClip { clip_id, new_start });
            let mut session = session_move.borrow_mut();
            if let Err(e) = session.submit(&cmd) {
                warn!(?e, "move clip rejected");
                if let Some(editor) = weak_move.upgrade() {
                    publish_project(&editor, &session_move, &selection_move);
                }
            }
        });
}

fn publish_project(
    editor: &EditorWindow,
    session: &Rc<RefCell<Session>>,
    selection: &Selection,
) {
    let selected = *selection.borrow();
    let dto = project_to_ui(session.borrow().project(), selected);
    editor.global::<AppState>().set_project(dto);
}

fn parse_clip_id(s: &slint::SharedString) -> Option<ClipId> {
    if s.is_empty() {
        None
    } else {
        ClipId::from_str(s.as_str()).ok()
    }
}

fn sec_to_timeline_time(
    session: &Rc<RefCell<Session>>,
    sec: f32,
) -> Result<RationalTime, ()> {
    let timebase = session.borrow().project().sequence.timebase;
    if timebase == 0 {
        return Err(());
    }
    let num = (sec.max(0.0) * timebase as f32).round() as i64;
    Ok(RationalTime::new_raw(num, timebase))
}
