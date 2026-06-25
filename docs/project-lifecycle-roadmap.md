# Project Lifecycle Roadmap — save, open, never lose work

> **Update (app-owned drafts — supersedes Phases 1–4 below).** Cutlass moved
> off the explicit file-based model to the CapCut-style one this doc deferred:
> the app now **owns every project**. There is no user-chosen `.cutlass` path,
> no manual Save / Save As, no dirty dot, and no unsaved-changes or
> crash-recovery prompt — there's nothing to lose because every edit
> auto-saves.
>
> - **Storage.** Each project is a directory under `projects/` in the per-user
>   data dir, holding `project.cutlass` (read/written through the unchanged
>   engine `Save`/`Load`, so the path-keyed plumbing below is reused as-is) and
>   a `meta.json` sidecar caching the display name for the gallery
>   (`crates/cutlass-ui/src/drafts.rs`).
> - **Cadence.** The worker debounces edits (~300 ms idle) into one write, with
>   an immediate flush on session swap and on close. Scrub/seek frames don't
>   defer the flush (`preview_worker.rs`, `PERSIST_DEBOUNCE`).
> - **UI.** The launch screen is a project gallery (open / delete per row); the
>   title bar renames the live project inline
>   (`EditCommand::SetProjectName`, undoable). **Open file…** imports an
>   external `.cutlass` into a new draft; **Export** is the way out.
> - **Removed.** `Engine::restore_session`, `src/autosave.rs`, `src/recent.rs`,
>   the Save As / Open Recent menu items, and the General settings pane.
>
> Phase 5 (missing-media relink) and the architecture invariants still stand.
> The shipped-✅ phases are kept below as history of how we got here.

Policy: **we follow CapCut** — but lifecycle is the one area where we follow
the *desktop platform* first: Cmd/Ctrl+S saves, a dot in the title marks
unsaved changes, closing with unsaved work asks, and a crash never costs
more than a few seconds of edits. CapCut hides most of this behind
autosave + a project home screen; we ship the explicit file-based model
first (local-first: a project is a `.cutlass` file the user owns), then
layer autosave and recents on top.

This doc tracks v1-roadmap **M0 — "Stop the bleeding"**'s lifecycle items:
the engine has had `Save`/`Open`/`Load` since the headless days
(`ProjectCommand`, `crates/cutlass-engine/src/action/project/`); the UI has
never exposed them. Sessions are ephemeral today — the worst gap in the
product.

## Architecture invariants (apply to every phase)

- **The engine is the single source of truth** — including for save state.
  Saving and opening are `ProjectCommand`s applied on the worker thread;
  the UI learns the result from the republished projection, never by
  tracking its own bookkeeping.
- **Dirty is an engine fact, not a UI guess.** The engine owns a session
  *revision counter* (bumped on every successful project mutation: edits,
  imports, undo, redo) and the revision last saved; `is_dirty()` compares
  them. The worker republishes it with every projection, so the title-bar
  dot can never disagree with history.
- **File dialogs are async** (`rfd::AsyncFileDialog` + `slint::spawn_local`,
  the import pattern in `src/main.rs`) — the blocking variant re-enters
  Slint's timer processing on macOS and aborts.
- **Lifecycle commands are not undoable** (matching the engine: open/load
  clear history; save records nothing). Undo never un-saves a file.
- **Destructive transitions always ask.** Open/New/Close with unsaved
  changes route through one unsaved-changes guard — one dialog, one
  policy, every entry point.

## Status legend

- [x] shipped
- [ ] not started / in progress

---

## Phase 0 — Foundation (done)

What this builds on — all engine-side, all tested:

- [x] `ProjectCommand::{Save, Open, Load}` dispatched by the engine:
      save writes pretty JSON `.cutlass` (schema v1) and records
      `project_path`; open replaces the session strictly (every media
      path must exist); load tolerates missing media
      (`action/project/{save,open,load}.rs`, `tests/project_file.rs`).
- [x] Open/Load clear undo history and reset the decoder pool; the frame
      cache re-registers every media source (`load_session`).
- [x] Worker thread owns the engine; every mutation republishes the
      projection (`publish_projection`) — the chokepoint session state
      rides on.
- [x] Title bar renders a centered project title from `EditorStore`
      (`ui/shell/title-bar.slint`).
- [x] Async file-dialog pattern established by import
      (`pick_import_path` in `src/main.rs`).

## Phase 1 — Save, Save As, dirty state ✅

The smallest slice that ends data loss for a user who saves: Cmd/Ctrl+S
works, the title tells the truth, and a `.cutlass` file round-trips
through the existing engine commands.

- [x] **Engine revision counter**: `Engine::revision()` bumps on every
      successful mutating apply (edit commands, import, open/load) and on
      undo/redo; `Save` records the saved revision; `Engine::is_dirty()`
      compares. Open/Load rebaseline as clean. Unit-tested alongside the
      existing save/open round-trip tests (`tests/project_file.rs`).
      (Deliberate conservatism: a rolled-back gesture or an undo back to
      the saved state still reads dirty — false-positives only, never a
      false "saved".)
- [x] **Worker save path**: `WorkerMsg::SaveProject { path: Option<PathBuf> }`
      — `None` reuses the engine's `project_path` (plain Cmd+S on a saved
      project), `Some` rebinds it (first save / Save As). Applied in the
      ordered mutation lane (never coalesced away); republishes the
      projection on success so the dot clears.
- [x] **Projection carries save state**: `project-dirty`,
      `project-has-path`, `project-file-name` (file stem) on
      `EditorStore`, set by `publish_projection` from engine facts.
- [x] **Shortcuts**: Cmd/Ctrl+S saves (path picker on first save),
      Cmd/Ctrl+Shift+S is Save As — in the window `FocusScope`'s
      command-modifier block (`app.slint`), like every shortcut.
- [x] **Save button** in the title bar (quiet `SubtleButton` next to
      Export), enabled while dirty or never-saved — the discoverable
      twin of the shortcut.
- [x] **Title-bar dirty dot**: `● name` while unsaved changes exist; the
      title shows the file stem once the project has a file, falling back
      to the project title before that.
- [x] **Save dialog**: async `.cutlass`-filtered save panel; the chosen
      path gets the `.cutlass` extension appended when missing (typed
      "v1.2" becomes "v1.2.cutlass", never "v1.cutlass"); Save As
      defaults to the current file name.

Deliberate gaps (Phase 1 ships without): no unsaved-changes guard on
close/open yet (Phase 2), save failures only log + leave the dot on (a
visible error surface is Phase 2 polish with the guard dialog), no
project title rename UI.

## Phase 2 — Open, New, and the unsaved-changes guard ✅

The other half of the lifecycle: getting projects back, starting fresh,
and never losing work to a careless close.

- [x] Cmd/Ctrl+O → async open dialog → `WorkerMsg::OpenProject`. Worker
      applies `ProjectCommand::Open`; on success it re-registers every
      pool media with the thumbnail and strip workers (the import-path
      bookkeeping, factored into `register_media_with_workers`),
      republishes everything, and bumps `EditorStore.session-epoch` — the
      `app.slint` watcher resets session state (pause, clear selection,
      clear in/out range, playhead to 0).
- [x] Open failure surfaces to the user (missing media = the strict-open
      error today): the worker sets `EditorStore.session-error`, which
      mounts an in-window `MessageDialog` (export-dialog mold,
      `ui/shell/message-dialog.slint`) naming the offending path — the
      relink flow is its own roadmap item (Phase 5); until then the
      message is honest. Save failures surface the same way (closing the
      Phase 1 gap).
- [x] Cmd/Ctrl+N → New project: `Engine::new_session()` replaces the
      session with an empty project (history cleared, decoders dropped,
      path unbound, rebaselined clean) — engine-tested alongside the
      save/open round-trips.
- [x] **One unsaved-changes guard** for every destructive transition
      (Open, New, window close — title-bar ✕ and OS close-request alike):
      native Save / Don't Save / Cancel dialog (`request_transition` in
      `src/main.rs`). Save parks the transition, routes through the Phase 1
      save path (including the first-save picker), and the worker's
      `save-finished(ok)` signal continues it on success — a failed save
      or a cancelled picker aborts the transition. One transition at a
      time (guard-open + pending locks).
- [x] Window close interception: `WindowBackend.close()` and the winit
      close-request (`Window::on_close_requested`, answering
      `KeepWindowShown`) both consult the guard before `quit_event_loop`.

## Phase 3 — Recent projects ✅

- [x] MRU list (last 10 `.cutlass` paths, newest first) persisted in the
      user config dir (`~/.cutlass/recent.json`): the worker notes every
      *successful* save and open — the moments a path is proven real —
      and republishes `EditorStore.recent-projects`; main.rs seeds the
      list at launch; missing files are pruned on read so the UI never
      offers a dead path (`src/recent.rs`, unit-tested).
- [x] Surfaced in the UI: a File menu on the title bar (New Project /
      Open… / Open Recent ▸ / Save / Save As…) — a native Slint
      `ContextMenuArea` under a quiet File button next to the brand.
      Phase 1's standalone Save button folded into the menu as planned
      (still gated on dirty-or-never-saved; the title-bar dot stays the
      save-state surface). Open Recent routes a known path through the
      same unsaved-changes guard as Cmd+O, just without the picker; a
      file deleted since the list was read fails like any open and
      surfaces in the session-error dialog.
- [x] Optional polish: empty-session welcome state in the library panel
      (CapCut-home-lite): a fresh session (no media, no tracks) shows
      "Get started" — New project / Open… buttons and the clickable
      recents list — where the media grid will live.

## Phase 4 — Autosave & crash recovery ✅

- [x] Timer-driven autosave of dirty sessions to a sidecar
      (`~/.cutlass/autosave/`, never to the user's file): a 30 s UI timer
      sends a sweep to the worker, which snapshots when dirty and the
      content actually changed (slot remembers the engine revision it
      captured — a dirty-but-idle session never rewrites), and removes
      the slot when clean (just saved / untouched). Worst case loss: 30 s.
      Slot identity (`src/autosave.rs`, unit-tested): saved projects hash
      their absolute path (survives a crash), unsaved sessions key on the
      pid (orphaned by a crash — which is what recovery looks for). A
      `.meta.json` sidecar names the source file, written *after* the
      snapshot so a torn write degrades to "no candidate".
- [x] Crash recovery: on launch, the newest slot worth offering — an
      orphan from an unsaved session, a slot newer than its source, or
      one whose source is gone — gets a native Restore / Discard dialog.
      Restore is `Engine::restore_session` (engine-tested): tolerant
      load, session bound to the *source* path (not the sidecar), left
      dirty so Cmd+S writes the recovered work back to the real file;
      media re-register with the tile workers and the session epoch
      resets UI state, same as an open. "Don't Save" on close discards
      the slot too — explicitly thrown-away work is never offered back.
- [x] Autosave uses the same serializer (`Project::save_to_file`) in the
      worker's ordered mutation lane; failures log and never interrupt
      editing. (Benchmark note stands: revisit with a snapshot +
      background write only if a huge project ever makes the JSON write
      visible next to playback.)

## Phase 5 — Missing-media relink

Tracked as its own M0 item; lands here because open is its entry point.

- [ ] Open switches to `Load` semantics behind a relink dialog: missing
      media listed, each entry re-pointable to a new path (file dialog),
      "locate folder" applying one directory substitution to all misses.
- [ ] Relinked paths re-validate (probe dimensions/duration against the
      stored `MediaSource`) before the project goes live.
- [ ] Offline media renders as a placeholder (slate frame) instead of
      failing the open — the strict/tolerant split the engine already
      has (`Open` vs `Load`) becomes a UI policy choice.

---

## Known gaps / tech debt

- The revision model reads "dirty" after undoing back to the exact saved
  state (no content hashing). Cheap to live with; revisit only if users
  complain.
- `Project.title` and the file name are independent (CapCut names
  projects; we name files). A rename affordance — and whether save-as
  retitles the project — remains unbuilt; the Phase 3 File menu is where
  it would live.
- No "Clear Recent Projects" menu item: entries leave the MRU only by
  falling off the 10-entry cap or their file disappearing. Add to the
  Open Recent submenu if anyone asks.
- Autosave of huge projects is an unmeasured cost (the revision check
  caps it at one JSON write per actual change, but a single write on the
  worker thread still steals from preview); profile before optimizing.
- The launch recovery offer takes the newest candidate only; older
  orphans stay on disk until they're offered (or restored sessions sweep
  them into a live slot). Bounded by crashes-per-machine, fine for v1.
- A sweep racing a "Don't Save" close can in principle resurrect a slot
  the UI just deleted (the worker write lands after the UI-thread
  discard). Window is milliseconds every 30 s; worst case is one stale
  restore offer next launch.
- The native unsaved-changes dialog is not app-modal: the editor stays
  interactive behind it, so edits made while it's up ride along with
  whichever choice lands. Harmless in practice; an in-window modal (the
  `MessageDialog` pattern) is the fix if it ever confuses anyone.
- Zoom-to-fit after open is not wired (the viewport width lives in
  `TimelinePanel`, the epoch watcher at window scope) — the timeline
  opens at default zoom; Fit is one click away. Polish later.
- macOS Cmd+Q quits through the app menu and bypasses the close-request
  guard. Needs a `winit` quit-intercept or menu rewiring — platform
  follow-up.
