//! Editor session — the single chokepoint between *something asking for an
//! edit* and *the project actually changing*.
//!
//! The decoder/engine/timeline/models split is intentional: `timeline::apply`
//! is the pure-data atom that mutates a [`Project`]; `Engine` is the media
//! service that decodes frames. Neither of them owns "the live editor
//! state". That's this layer.
//!
//! Today the layer's responsibilities are deliberately small:
//!
//! 1. **Own the authoritative `Project`.** All mutations route through here.
//! 2. **Funnel `timeline::Command`s into `timeline::apply`** via
//!    [`Session::submit`].
//! 3. **Stash `CommandEffect`s** for the future history layer (no `undo`
//!    yet — see the note on [`History`]).
//! 4. **Notify a listener on every change** via the `on_changed` callback
//!    the caller installs at construction. The Slint shell uses this to
//!    rebuild the DTO and push it onto `AppState.project`.
//!
//! Things this layer pointedly **does not** do (yet, by design):
//!
//! * Talk to `Engine`. The reactive "AddClip → open the media → preheat the
//!   decoder" plumbing is the obvious next layer, but it's its own design
//!   conversation (which decoder owns the source? lifecycle on RemoveClip?
//!   pool eviction?) and bolting it onto the same chokepoint hides that
//!   conversation behind a wall of code. Leaving a clean seam.
//! * Push incremental updates (per-clip / per-track diffs) into Slint. The
//!   `on_changed` callback receives the whole `Project`. Once the
//!   media-bin grid or track list grows large enough to make full-DTO
//!   refresh visibly janky, swap the single callback for finer-grained
//!   events. The Slint state already has `ModelRc` plumbing that supports
//!   incremental rows; the wiring just has to land.
//!
//! ## Threading
//!
//! `Session` is **not `Send` / not `Sync`** by construction (the callback
//! closure is `FnMut` and the Slint side wants UI-thread mutation anyway).
//! Hold it inside an `Rc<RefCell<Session>>` if multiple UI callbacks need
//! access (this is what `main.rs` does).

// The chokepoint methods (`submit`, `history`, undo stubs, etc.) are the
// public surface this layer is *meant* to grow into. Right now `main.rs`
// only calls `add_media` (via the import path); the timeline commands
// won't flow through the Slint side until the editor UI grows the
// gestures that emit them. The in-module tests below exercise the rest,
// but the bin target's dead-code analysis doesn't see those. Allow at
// the module level so we don't litter the file with attributes; the
// followup PR that wires editor gestures will burn through these.
#![allow(dead_code)]

use models::{MediaSource, Project};
use timeline::{Command, CommandEffect, TimelineError};

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Append-only log of `(command, effect)` tuples produced by successful
/// [`Session::submit`] calls.
///
/// **Why no `undo()` yet?** Inverting an effect requires either a stored
/// "inverse command" (e.g. `RemoveTrack` to undo `AddTrack`) or an explicit
/// snapshot restore. The timeline crate's command vocabulary is currently
/// incomplete on the inverse side — there's no `RemoveTrack`, no
/// `JoinClips` to undo a split, etc. Implementing undo here without those
/// would only work for a subset, and a partially-working undo button is
/// worse UX than no undo button. So: this struct *captures the data* a
/// future history layer will need (the [`Command`] for replay/redo, the
/// [`CommandEffect`] for the inverse-data path), and `can_undo()` returns
/// `false` until the timeline crate grows the missing inverses.
#[derive(Debug, Default)]
pub struct History {
    past: Vec<HistoryEntry>,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub command: Command,
    pub effect: CommandEffect,
}

impl History {
    /// Number of recorded edits.
    #[inline]
    pub fn len(&self) -> usize {
        self.past.len()
    }

    /// `true` when no edits have been recorded.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.past.is_empty()
    }

    /// Most recent recorded edit, if any.
    #[inline]
    pub fn last(&self) -> Option<&HistoryEntry> {
        self.past.last()
    }

    /// Stub. Always `false` until the timeline crate grows inverse
    /// commands; see the type-level doc.
    #[inline]
    pub fn can_undo(&self) -> bool {
        false
    }

    /// Stub. Symmetric with `can_undo`.
    #[inline]
    pub fn can_redo(&self) -> bool {
        false
    }

    fn push(&mut self, command: Command, effect: CommandEffect) {
        self.past.push(HistoryEntry { command, effect });
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// The editor brain. Owns the live [`Project`], funnels every mutation
/// through one place, and notifies one listener on each change.
///
/// Construction takes the initial project plus an `on_changed` callback.
/// The callback fires **once on construction** (so the listener can seed
/// its view) and then after every successful mutation. It does **not**
/// fire on a failed [`Session::submit`] — failures leave the project
/// byte-identical to before the call, so there's nothing for the listener
/// to refresh.
pub struct Session {
    project: Project,
    history: History,
    on_changed: Box<dyn FnMut(&Project)>,
}

impl Session {
    /// Build a session around `project`. The `on_changed` callback fires
    /// immediately with `&project` so the caller can publish the initial
    /// state without a separate seeding call.
    pub fn new(project: Project, on_changed: impl FnMut(&Project) + 'static) -> Self {
        let mut session = Self {
            project,
            history: History::default(),
            on_changed: Box::new(on_changed),
        };
        session.notify();
        session
    }

    /// Run one structured edit. On `Ok`, the project has changed,
    /// `is_dirty` is set, the effect is stashed in [`History`], and the
    /// `on_changed` listener has been notified. On `Err`, none of those
    /// happen — the project is unchanged.
    pub fn submit(&mut self, command: &Command) -> Result<(), TimelineError> {
        let effect = timeline::apply(&mut self.project, command)?;
        self.history.push(command.clone(), effect);
        self.project.is_dirty = true;
        self.notify();
        Ok(())
    }

    /// Append a probed media source to the project's bin.
    ///
    /// This deliberately isn't a `timeline::Command` (yet) — media import
    /// flow is orthogonal to the AI-emittable edit vocabulary, and folding
    /// `AddMedia` into the timeline crate's `Command` enum without a clear
    /// undo/replay story would muddle that vocabulary. If we later decide
    /// "imports are an edit" (e.g. so the agent can `add_media` deterministically),
    /// promote this to a real command at that point.
    pub fn add_media(&mut self, media: MediaSource) {
        self.project.media_bin.push(media);
        self.project.is_dirty = true;
        self.notify();
    }

    /// Authoritative view of the current project. Borrow-only — every
    /// mutation has to go through [`Self::submit`] / [`Self::add_media`].
    #[inline]
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Read-only access to the history log.
    #[inline]
    pub fn history(&self) -> &History {
        &self.history
    }

    fn notify(&mut self) {
        (self.on_changed)(&self.project);
    }
}

// ---------------------------------------------------------------------------
// Tests — headless, no Slint dep.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use models::{
        Clip, ClipId, Color, MediaId, MediaKind, MediaSource, Project, ProjectId,
        Rational, RationalTime, SchemaVersion, Sequence, SequenceId, TrackId, TrackKind,
    };
    use timeline::{AddClip, AddTrack, Command, TimelineError};

    use super::Session;

    const TB: u32 = 90_000;

    fn rt(num: i64) -> RationalTime {
        RationalTime::new_raw(num, TB)
    }

    fn empty_project() -> Project {
        Project {
            id: ProjectId::new(),
            name: "Test".into(),
            file_path: None,
            schema: SchemaVersion::CURRENT,
            sequence: Sequence {
                id: SequenceId::new(),
                name: "S".into(),
                width: 1920,
                height: 1080,
                fps: Rational::new_raw(30, 1),
                sample_rate: 48_000,
                timebase: TB,
                duration: rt(0),
                in_point: None,
                out_point: None,
                tracks: vec![],
            },
            media_bin: vec![],
            is_dirty: false,
        }
    }

    fn dummy_media() -> MediaSource {
        MediaSource {
            id: MediaId::new(),
            name: "dummy.mp4".into(),
            path: "/dev/null".into(),
            kind: MediaKind::Video,
            has_video: true,
            has_audio: false,
            duration: rt(TB as i64),
            video: None,
            audio: None,
            is_supported: true,
            is_loading: false,
            is_missing: false,
            error: None,
        }
    }

    /// Build a session with a notification counter. The counter increments
    /// on every `on_changed` call — including the implicit one inside
    /// `new()`, which is the seeding callback the Slint shell relies on.
    fn session_with_counter(project: Project) -> (Session, Rc<RefCell<usize>>) {
        let n = Rc::new(RefCell::new(0usize));
        let n_cb = n.clone();
        let s = Session::new(project, move |_p| {
            *n_cb.borrow_mut() += 1;
        });
        (s, n)
    }

    #[test]
    fn new_fires_initial_on_changed() {
        let (_s, n) = session_with_counter(empty_project());
        assert_eq!(*n.borrow(), 1, "on_changed must fire once on construction");
    }

    #[test]
    fn submit_happy_path_mutates_marks_dirty_and_notifies() {
        let (mut s, n) = session_with_counter(empty_project());
        let track_id = TrackId::new();

        s.submit(&Command::AddTrack(AddTrack {
            track_id,
            kind: TrackKind::Video,
            name: "V1".into(),
            height_px: None,
        }))
        .expect("AddTrack must succeed on an empty sequence");

        assert_eq!(s.project().sequence.tracks.len(), 1);
        assert_eq!(s.project().sequence.tracks[0].id, track_id);
        assert!(s.project().is_dirty, "successful submit must mark dirty");
        assert_eq!(s.history().len(), 1);
        // 1 from new(), 1 from submit().
        assert_eq!(*n.borrow(), 2);
    }

    #[test]
    fn submit_error_leaves_project_unchanged_and_skips_notify() {
        let (mut s, n) = session_with_counter(empty_project());
        let before = s.project().clone();
        let starting_notifies = *n.borrow();

        // AddClip without first adding a track → TrackNotFound.
        let bad_track = TrackId::new();
        let result = s.submit(&Command::AddClip(AddClip {
            track_id: bad_track,
            clip: Clip {
                id: ClipId::new(),
                media_id: None,
                track_id: bad_track,
                name: "x".into(),
                start: rt(0),
                duration: rt(TB as i64),
                source_in: rt(0),
                source_out: rt(TB as i64),
                speed: Rational::ONE,
                opacity: 1.0,
                volume: 1.0,
                enabled: true,
                color: Color::rgb(0, 0, 0),
            },
        }));
        assert!(matches!(result, Err(TimelineError::TrackNotFound(_))));

        // Project must be byte-identical to before the failed call, no
        // history entry, no notify.
        assert_eq!(s.project().sequence.tracks.len(), before.sequence.tracks.len());
        assert_eq!(s.project().is_dirty, before.is_dirty);
        assert_eq!(s.history().len(), 0);
        assert_eq!(*n.borrow(), starting_notifies);
    }

    #[test]
    fn add_media_appends_marks_dirty_and_notifies_but_does_not_push_history() {
        let (mut s, n) = session_with_counter(empty_project());
        let media = dummy_media();
        let id = media.id;

        s.add_media(media);

        assert_eq!(s.project().media_bin.len(), 1);
        assert_eq!(s.project().media_bin[0].id, id);
        assert!(s.project().is_dirty);
        // History only tracks `timeline::Command`s. Media imports are not
        // (yet) part of that vocabulary; verify the boundary.
        assert_eq!(s.history().len(), 0);
        assert_eq!(*n.borrow(), 2, "construction + add_media");
    }

    #[test]
    fn history_stores_command_and_effect_in_order() {
        let (mut s, _n) = session_with_counter(empty_project());
        let t1 = TrackId::new();
        let t2 = TrackId::new();
        s.submit(&Command::AddTrack(AddTrack {
            track_id: t1,
            kind: TrackKind::Video,
            name: "V1".into(),
            height_px: None,
        }))
        .unwrap();
        s.submit(&Command::AddTrack(AddTrack {
            track_id: t2,
            kind: TrackKind::Audio,
            name: "A1".into(),
            height_px: None,
        }))
        .unwrap();

        assert_eq!(s.history().len(), 2);
        // Stub semantics — keep the assertion so we notice when undo lands.
        assert!(!s.history().can_undo());
        assert!(!s.history().can_redo());
    }
}
