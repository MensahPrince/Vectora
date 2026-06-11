# Timeline Roadmap — CapCut-style, end to end

Policy: **we follow CapCut.** When a UX question comes up, the answer is
"what does CapCut desktop do?" — magnet snapping with guide lines, kind-segregated
lanes, auto-created/auto-removed tracks, a magnetic main track, ghost previews
that never lie.

This doc tracks the path from today's timeline to that target. Phases are
ordered so each ships something usable on its own.

## Architecture invariants (apply to every phase)

These patterns are already established — new timeline work should follow them
rather than invent parallel mechanisms:

- **The engine is the single source of truth.** UI gestures end in a
  `cutlass_commands::EditCommand` applied on the worker thread
  (`crates/cutlass-ui/src/preview_worker.rs`); the UI re-renders from the
  republished projection (`crates/cutlass-ui/src/projection.rs`). No Slint-side
  mutation of project state, ever.
- **One resolver per gesture, shared by preview and commit.** Placement logic
  lives in a Rust pure callback (`crates/cutlass-ui/src/snap.rs`, exposed via
  `ui/lib/drag-backend.slint`). The ghost, the guides, and the release commit
  all read the *same* resolution, so the preview is exactly what a release
  does. Trim, ripple, etc. get the same treatment.
- **Gesture state is recorded by the grabbed element, resolved by the panel.**
  `ClipView` only snapshots the press + cursor deltas into `TimelineViewState`;
  `TimelinePanel` owns resolution, visuals, and teardown
  (`ui/panels/timeline/timeline.slint`).
- **Lane list is stack top-first.** Top lane = front compositing layer
  (CapCut/Premiere convention). UI row `r` ↔ engine order index
  `track_count − 1 − r`; inserting so a lane appears at row `r` means engine
  index `(len − r).clamp(0, len)`.
- **Every command is undoable.** New `EditCommand`s need an inverse action
  (`crates/cutlass-engine/src/action/edit/`).
- **Perf:** drag-frame resolvers are hot paths — keep them allocation-light and
  O(total clips) or better; decode/thumbnail work never blocks the UI thread.

## Status legend

- [x] shipped
- [ ] not started / in progress

---

## Phase 0 — Foundation (done)

- [x] Engine command surface: `AddTrack` (with stack index), `AddClip`,
      `AddGenerated`, `SplitClip`, `TrimClip`, `MoveClip`, `RemoveClip`,
      `RemoveTrack`, `RippleDelete` — all with undo inverses.
- [x] Worker thread owns the engine; scrub frames coalesce, mutations never
      dropped; projection republished after every edit.
- [x] Lane list renders stack top-first; per-kind lane colors.

## Phase 1 — Media drops (done)

- [x] Drag library tile → timeline with window-level ghost (real duration width).
- [x] Drop on a video lane lands there; occupied spans slide right into the
      first gap that fits (`first_fit_start`).
- [x] Drop on empty space / foreign lane creates a video track at the drop row
      (`create_track`), named per kind (V1, V2, …).
- [x] Drop position snaps (clip edges, playhead, tick 0).

## Phase 2 — Clip move, snapping, guides (done)

- [x] Free x/y drag: floating copy follows the cursor, original dims in place.
- [x] Magnet snapping to clip edges on all lanes, the playhead, and tick 0;
      vertical guide line at the snap tick; toolbar **Snap** toggle
      (`TimelineStore.snap-enabled`).
- [x] Landing ghost shows the exact release position; conflicts and
      foreign-kind lanes resolve to a **new lane** with a horizontal insertion
      line at the hovered row (kinds never mix).
- [x] Cross-lane moves that empty their source lane remove it
      (CapCut deletes emptied overlay tracks).
- [x] No-op drags (click without move) commit nothing.

## Phase 3 — Trim (edge drag) ✅

The next gesture CapCut users reach for.

- [x] Trim handles on the clip's left/right edges (`ew-resize` cursor, ~6px
      hit zones capped at ⅓ of the clip width in `clip.slint`; bracket bars
      on selection/hover).
- [x] `resolve_clip_trim` pure callback in `snap.rs`: clamps to source media
      bounds (the projection computes per-edge `head/tail-room-ticks`,
      rate-converted conservatively so the engine can never reject a
      UI-offered extension), neighbor edges on the lane, tick 0, and a 1-tick
      minimum; magnets the dragged edge to the same snap candidates and
      reuses the vertical guide line (snaps the clamp rejects are dropped).
- [x] Live preview: opaque stretch rect over the dimmed original during the
      gesture; commit `EditCommand::TrimClip` on release (engine still
      validates source-out-of-bounds and overlap atomically).
- [x] CapCut detail: duration + signed-delta tooltip above the dragged edge.

## Phase 4 — Playhead, ruler, scrubbing ✅

- [x] CapCut-style ruler, rebuilt from scratch (`src/ruler.rs` +
      `ruler.slint`): compact `MM:SS` labels centered on their position
      (no tick line — the text is the marker), `Nf` frame labels between
      second boundaries at deep zoom, dot subdivisions instead of hash
      marks, pin-shaped playhead head. Adaptive ladder runs on integer
      frames against the *nominal* fps (frame steps must divide it, so
      second boundaries always stay labeled); marks are virtualized to
      the viewport and capped.
- [x] Click/drag on the ruler moves the playhead (replaced the temporary
      toolbar slider as the scrub control; playhead changes funnel
      through one `TimelinePanel` watcher into coalesced frame requests).
- [x] Scrubbing snaps the playhead to clip edges / tick 0 when the magnet
      is on (same resolver as clip drags, zero-width span).
- [x] Keyboard: ←/→ frame step, Home/End.
- [x] Toolbar zoom slider (log scale, anchored on the playhead / viewport
      center) so the adaptive ruler is reachable; Ctrl+scroll zoom stays
      in Phase 9.
- [x] Preview frame requests keep coalescing through the worker
      (`WorkerMsg::Frame`).

## Phase 5 — Selection ops & shortcuts ✅

Commands existed in the engine; this was UI wiring. All shortcuts accept
Ctrl and Cmd (macOS); they live in a window-level `FocusScope`
(`app.slint`) and route through `TimelineActions`, the same functions
the toolbar buttons call, so gating can never diverge. Timeline
interactions bump a refocus nonce so shortcuts reclaim the keyboard
after a text input had it.

- [x] Delete/Backspace → `RemoveClip` (+ auto-remove emptied lane, same
      helper as moves); toolbar **Delete** button.
- [x] Split at playhead: toolbar button + Ctrl/Cmd+B → `SplitClip`, gated
      on the playhead being strictly inside the selected clip.
- [x] Undo/redo: Ctrl/Cmd+Z / +Shift+Z → engine history; toolbar buttons
      driven by `can-undo`/`can-redo` republished with every projection.
- [x] Copy/paste/duplicate: Ctrl/Cmd+C/V/D. Copy snapshots the clip's
      *content* on the worker (survives deleting the original); paste
      lands at the playhead on the source lane, first-fit sliding right
      (same policy as drops; recreates the lane if it's gone); duplicate
      places the copy right after the original.
- [x] Esc clears selection (empty-lane click already did).

## Phase 6 — Compound undo (one gesture = one history entry)

A new-lane move currently records up to three entries (`AddTrack` + `MoveClip`
+ `RemoveTrack`), and a delete that empties its lane records two
(`RemoveClip` + `RemoveTrack`); one Ctrl+Z should revert the whole gesture.

- [ ] Engine: transaction/grouping in `history` (begin/commit around a
      dispatched batch, inverses applied in reverse order).
- [ ] Worker: wrap multi-command gestures (new-lane move, drop-with-new-lane,
      future ripple ops) in one group.

## Phase 7 — Main-track magnet (ripple)

CapCut's signature behavior; needs design care, ship behind its own toggle
(separate from Snap, as in CapCut).

- [ ] Designate a **main track** (bottom video lane). With main-track magnet
      on: clips on it pack left (no gaps), drops/moves *insert* and shift
      later clips right, deletions close the gap (`RippleDelete` exists;
      ripple-insert does not).
- [ ] Engine: `RippleInsert` / shift-right primitives with inverses.
- [ ] Drag UX on the main lane: insertion index ghost (between-clip caret)
      instead of free positioning.
- [ ] Off state = today's freeform behavior.

## Phase 8 — Clip content rendering

Perf-sensitive; everything decoded off the UI thread and cached.

- [ ] Video clips: filmstrip thumbnails (sample frames at zoom-dependent
      density; cache per media + zoom bucket; never decode on the UI thread).
- [ ] Audio clips: waveform strips (peak files computed once per media,
      rendered per zoom).
- [ ] Text clips: render `text-content` inline (basic version exists via name
      label).
- [ ] Clip badges: duration, speed, volume markers as they land in the model.

## Phase 9 — Drag & viewport polish

- [ ] Auto-scroll when dragging near the viewport edges (CapCut scrolls the
      timeline under the drag; applies to clip moves, trims, and library
      drops).
- [ ] Snap guides for library drags (the window-level ghost currently doesn't
      show the vertical guide the resolver already computes).
- [ ] Zoom-to-fit button + Ctrl+scroll zoom centered on the cursor (the
      toolbar slider with playhead/center anchoring landed in Phase 4).
- [ ] Timecode tooltip while dragging/trimming.
- [ ] Track headers: mute/lock/hide toggles (engine `Track.enabled` exists).

## Phase 10 — Multi-clip & linking

- [ ] Multi-select: shift-click, marquee; group move with one resolution
      (collision policy: reject or new-lane the whole set, CapCut-style).
- [ ] Linked video+audio from the same media (CapCut "linkage" toggle):
      import drops create linked pairs once audio tracks land; linked clips
      move/trim together.
- [ ] Compound clips (select N clips → one nested clip) — far future.

## Phase 11 — Transitions & effects on the timeline

- [ ] Transition drop targets at clip junctions (only when clips abut).
- [ ] Effect/filter/adjustment lanes already exist as kinds; drag-drop from a
      future effects panel follows the Phase 1/2 resolver pattern.

---

## Known gaps / tech debt

- `changed` callbacks defer one event-loop iteration — drop commits read
  final-position state, but keep this in mind for new gesture wiring.
- Slint tick model is `i32` (projection clamps engine `i64`); fine for
  realistic timelines, revisit if hour-scale 120fps projects appear.
- Selection is keyed `(track-id, clip-id)`; engine clip ids are globally
  unique, so this can simplify to clip id alone.
- Selection can go stale after undo/redo (the projection republish doesn't
  touch `TimelineStore`); stale ids resolve to "nothing selected"
  everywhere, but clearing selection on history steps would be cleaner.
- The clipboard lives on the worker thread (content snapshot, not a
  reference) — fine for clips, revisit when multi-select copy lands.
