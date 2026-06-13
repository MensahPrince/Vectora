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
      through one watcher into coalesced frame requests — moved to
      window scope when playback landed, see `playback-roadmap.md`).
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

## Phase 6 — Compound undo (one gesture = one history entry) ✅

A new-lane move used to record up to three entries (`AddTrack` + `MoveClip`
+ `RemoveTrack`), and a delete that emptied its lane two (`RemoveClip` +
`RemoveTrack`); one Ctrl+Z now reverts the whole gesture.

- [x] Engine: history groups (`Engine::begin_group` / `commit_group`) collect
      every inverse a dispatched batch records into one compound entry; undo
      applies them in reverse order, and the entry oscillates like any single
      action. Empty groups record nothing; single-command groups collapse to
      a plain entry.
- [x] `Engine::rollback_group` aborts a failed gesture: the collected
      inverses are applied in reverse on the spot, restoring the pre-gesture
      state and leaving history untouched — including the redo stack, so a
      failed gesture is a complete no-op. (Replaces the worker's hand-rolled
      "remove the lane we just created" compensation; failed drops now clean
      up their lane too.)
- [x] Worker: new-lane moves, drops that create a lane, deletes that empty
      their lane, and pastes that recreate a lane each commit as one group;
      future ripple ops should use the same wrapper.

## Phase 7 — Main-track magnet (ripple) ✅

CapCut's signature behavior, behind its own toolbar **Magnet** toggle
(separate from Snap, as in CapCut; on by default). Engine stays mechanism,
the magnet policy lives UI/worker-side.

- [x] Main track designation: the **bottom video lane** (engine: first video
      track in stack order; resolver: last video row). Computed, not stored —
      it follows lane creation/removal automatically.
- [x] Engine: `ShiftClips { track, from, delta }` ripple primitive (shift
      every clip starting ≥ `from`; validated atomically, exact-set inverse)
      and `RippleInsert { track, media, source, at }` (shift right + place,
      atomic with a compound inverse — built on Phase 6's `CompoundAction`).
- [x] With the magnet on, the main lane stays gapless: library drops
      `RippleInsert`; cross-lane moves in open a hole (`ShiftClips` + 
      `MoveClip`); reorders park-close-open-land as one group; moves *off*
      and `RippleDelete`s close the gap behind them; paste/duplicate insert
      at the nearest clip boundary / right after the original. Every gesture
      is one history entry (Phase 6 groups), rollback on failure.
- [x] Enabling the magnet packs the main lane (leading gap included) as one
      undoable entry — CapCut's lane is gapless the moment the toggle is on.
      The worker mirrors the flag (`SetMainMagnet`) for the ops that have no
      drag resolution (delete/paste/duplicate/pack).
- [x] Drag UX on the main lane: insertion caret between clips (slot picked
      by the dragged left edge vs clip midpoints) instead of free
      positioning, for clip drags and library drags alike. Reorders commit
      in post-close space; releasing on the clip's own slot is a no-op.
- [x] Off state = freeform behavior, unchanged everywhere else.

Deliberate gap — closed by v1 M0: **trims ripple now.** With the magnet
on, a trim touching the main lane composes `TrimClip` + `ShiftClips` per
lane in one history group (order depends on grow vs shrink: open room
before growing, trim before closing). The resolver gains the ripple mode
(no neighbor clamp; source headroom still binds), leading-edge trims
re-anchor at the old start, and linked members ripple on their own lanes
so A/V pairs stay in sync. Remaining polish: the drag stretch preview
shows the un-anchored extent (downstream doesn't shift live), and a
leading-edge grow on the lane's first clip is still clamped at tick 0.

## Phase 8 — Clip content rendering ✅

Perf-sensitive; everything decodes off the UI thread (`src/strips.rs` +
a dedicated `cutlass-strips` worker, newest request first) and lands in
UI-thread caches via a `StripBackend.generation` bump — the tile models
are pure callbacks that take it as an argument, so delivery re-evaluates
them automatically (same reactivity pattern as the ruler).

- [x] Video clips: filmstrip thumbnails. Tiles sit on a power-of-two grid
      of *media-time* seconds picked from the zoom (tile width ∈ [64, 128)
      px); powers of two nest, so zooming reuses every cached frame, and
      trims/moves slide the strip under the clip window (grid keys are
      media-anchored) instead of resampling. `cutlass_decoder::video_strip`
      decodes N targets in one demuxer pass, rolling forward between nearby
      targets and re-seeking past gaps; frames are decoded *to* the target
      pts so tiles inside one GOP differ. Resolvers are viewport-virtualized
      (clip-local 256px buckets from `ClipView`) and LRU-capped; misses
      render the lane-colored card until the frame lands.
- [x] Audio clips: waveform strips. A peak file (~100 peaks/s,
      `audio_peaks_per_second`) is computed once per media on first demand;
      tiles are rasterized per power-of-two zoom bucket on the worker
      (mirrored bars on a transparent ground over the lane color) and
      stretched ≤ 2× between buckets.
- [x] Text clips: `text-content` rendered inline (centered, elided;
      falls back to the lane label while empty).
- [x] Clip badges: name + duration (`3.4s` / `M:SS`, computed rate-exactly
      in the projection) on a thin top scrim, hidden on sliver-thin clips;
      the selection outline moved above the content tiles. The reserved
      slot filled when the M1 fields landed: a retime chip (`2x` / `0.5x
      R`) and an audio chip (struck-out speaker when muted, volume %, or
      a fade ramp) join the duration in a right-aligned badge cluster.

Deliberate gap: the drag floating copy and trim stretch preview stay flat
color — content in those gestures is a polish item for later.

## Phase 9 — Drag & viewport polish ✅

- [x] Auto-scroll when dragging near the viewport edges. A ~16ms `Timer` in
      `TimelinePanel` steps `scroll-x`/`scroll-y` while the cursor sits in a
      32px edge zone (speed ramps with depth), for clip moves, trims (x only),
      and library drops. Clip/trim deltas are TouchArea-local and Slint fires
      no `moved` while the cursor is still and content scrolls under it, so
      each step compensates `drag-dx/dy-px` / `trim-dx-px` by the scroll it
      actually applied (clamped at the bounds) — the ghost stays glued to the
      cursor. Library drops recompute their cursor tick reactively from
      `scroll-x`, so they need no compensation. `ClipView` records the
      window-space pointer (`TimelineViewState.pointer-window-*`).
- [x] Snap guides for library drags. `LibraryDropResolution` now carries
      `has-snap`/`snap-line-tick` (the freeform path already called
      `compute_drag_snap`), and the timeline draws the same `#00E5C7` vertical
      guide as clip drags. The over-timeline ghost is now *honest*: a
      content-space landing rectangle at the snapped, row-aligned spot inside
      the lanes — the window-level ghost (`app.slint`) only tracks the loose
      tile while outside the timeline.
- [x] Zoom-to-fit button + Ctrl/Cmd+scroll zoom centered on the cursor.
      `set-zoom` refactored into `set-zoom-anchored(zoom, anchor-tick)` (the
      Phase 4 anchoring math, now parameterized); the toolbar **Fit** button
      calls `zoom-to-fit` (viewport-w ÷ sequence duration, +5% tail margin).
      A `TouchArea` wrapping the lanes Flickable handles `scroll-event`:
      Ctrl/Cmd+wheel zooms anchored on the cursor tick, plain wheel scrolls
      lanes, Shift/horizontal-delta scrolls sideways (the lanes Flickable is
      `interactive: false`, so scroll bubbles out of the clip TouchAreas to
      this ancestor).
- [x] Timecode tooltip while dragging/trimming. The Phase 3 trim bubble is now
      a shared `DragTooltip`; clip moves and library drops show the landing
      timecode (`TimelineLib.format-timecode`, insert mode reads the caret
      slot), trim keeps its duration + signed-delta readout.
- [x] Track headers: hide/mute/lock toggles. Engine gained
      `SetTrackEnabled`/`SetTrackMuted`/`SetTrackLocked` commands over a shared
      `SetTrackFlagsAction` (snapshots all three flags → oscillating inverse)
      and a new `Track.locked` field; the worker applies them via
      `WorkerMsg::SetTrackFlag`. `TrackHead` renders the lane name + eye
      (visual) / speaker (audio) / lock toggles, projected from the engine
      (`enabled`/`muted`/`locked`). Hidden lanes dim their clips (folded into
      the existing per-element dim, never group `opacity`); locked lanes are
      read-only (clip TouchAreas disabled, resolvers skip them like a
      foreign-kind row, locking clears a selection living on the lane).
      `enabled=false` already drops visual tracks from `resolve_layers`, so the
      preview updates on the next scrub.

Deliberate gap: **mute is persisted + shown but silent-in-name only** — there
is no audio playback path yet (`Track.muted` is honored by no one), so the
speaker toggle is a stored flag awaiting the mixer (playback roadmap
Phase 3, see `playback-roadmap.md`).

## Phase 10 — Multi-clip & linking ✅

Selection became a set of clip ids plus a primary anchor
(`TimelineStore.selected-ids` + the existing `selected-track-id`/`-clip-id`,
kept consistent through `apply-selection`); the set logic lives in pure Rust
callbacks (`src/selection.rs`, exposed via `ui/lib/selection-backend.slint`),
same resolver-pattern as drags.

- [x] Multi-select: shift-click toggles a clip (and its link group) in the
      set (`pointer-event` modifiers in `clip.slint`; deselecting the primary
      re-anchors it on the first remaining clip in row/start order); marquee
      on empty lane space (`TrackLane` lost its TouchArea, so presses fall
      through to the panel's wheel-area: armed on press, live past a 3px dead
      zone, re-resolved per move, locked lanes skipped; a plain click still
      clears the selection). Delete removes the whole set as one history
      entry (`RemoveClips` batch, right-to-left so magnet ripples stay
      valid; emptied lanes removed after).
- [x] Group move with one resolution (`resolve_group_drag`): one uniform dx
      (clamped to tick 0, magneted on the grabbed clip's edges) + one row
      delta (forced to 0 when the set spans video+audio, so pairs stay in
      their zones), validated against everything outside the set — members
      can't collide with each other under a uniform shift. **Reject policy**,
      CapCut-style: weaker variants are tried (raw dx, horizontal-only) and
      if nothing fits the release commits nothing — floating copies flag it
      with a red outline, landing ghosts show the exact commit otherwise.
      The worker lands the batch park-then-place as one history group.
- [x] Linked video+audio (CapCut "linkage" toggle, on by default): engine
      grew `LinkId` + `Clip.link` (`serde(default)`, old projects load) and
      an undoable `LinkClips` command (fresh group, inverse restores prior
      links). With the toggle on, a video drop whose media has audio also
      lands an audio clip at the same tick (topmost unlocked audio lane with
      the span free, else a new bottom lane) and links the pair — one history
      entry. Linked clips select together (click/marquee pull partners in),
      move together (selection expansion ⇒ group resolver), trim together
      (`resolve_clip_trim` intersects every member's delta clamp and
      previews partner stretch ghosts; the worker replays the edge delta on
      the group), and split together (partners spanning the tick split too;
      tails re-linked as a fresh group). Toggle off ⇒ links persist, dormant.
- [ ] Compound clips (select N clips → one nested clip) — far future.

Group copy/duplicate and the unlink gesture landed with v1 M0: copy
snapshots the whole selection as one clipboard block (lanes + relative
placement; paste slides the block right as a unit until every member fits,
copied link groups re-link fresh), duplicate places the block right after
itself the same way, and the toolbar's Unlink button dissolves the
selection's link groups undoably (each member gets a fresh singleton group
via `LinkClips` — behaviorally unlinked everywhere links are read; a
dedicated `UnlinkClips` command can replace the trick once the command
surface is open again post-M1).

Deliberate gaps: **group moves and pastes are freeform** (no main-track
magnet ripple-insert for a multi-selection — single-clip paste/duplicate
keep it); linked pairs don't render a link badge yet.

## Phase 11 — Transitions & effects on the timeline

- [x] Transition pills at clip junctions (M4): rendered in content space
      centered on the cut, with an edge-drag resize (symmetric — the cut is
      the anchor, commits one `SetTransition`) and a remove control. Added
      from the Library Transitions tab onto the selected clip's right
      junction; the engine rejects junctions whose clips don't abut.
- [ ] Transition *drop targets* via drag-from-library (deferred — see
      `docs/effects-roadmap.md` § deliberate gaps; current flow is
      click-to-apply against the selection).
- [ ] Effect/filter/adjustment lane drag-drop from the library follows the
      Phase 1/2 resolver pattern (deferred with the above).

---

## Known gaps / tech debt

- `changed` callbacks defer one event-loop iteration — drop commits read
  final-position state, but keep this in mind for new gesture wiring.
- Slint tick model is `i32` (projection clamps engine `i64`); fine for
  realistic timelines, revisit if hour-scale 120fps projects appear.
- The multi-selection set is keyed by clip id alone (Phase 10), but the
  primary anchor is still the `(track-id, clip-id)` pair; the track half is
  redundant and could go.
- ~~Selection can go stale after undo/redo~~ — fixed in v1 M0: every
  projection republish prunes the selection against the new clip set
  (`SelectionBackend.prune`, called from the app's projection-revision
  watcher), dropping vanished ids and re-anchoring the primary.
- The clipboard lives on the worker thread (content snapshot, not a
  reference); since v1 M0 it holds the whole selection as one block.
