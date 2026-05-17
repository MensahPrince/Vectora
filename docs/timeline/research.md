# Timeline research

The **timeline** crate (working name — could be `project` or `model`; TBD) is the **brain** of Cutlass. It owns the **project state**: the timeline, the tracks, the clips, the media sources, undo history, and serialization. It is the layer that makes the engine + renderer into an *editor*.

This doc covers the **full vision**. The **MVP cutline** is explicit at the bottom and noted per-section.

---

## Mission and boundaries

**Timeline owns:**

- The **`Project`** root — the whole edit state, serializable.
- **Media sources** — `MediaSource { id: SourceId, original_path, proxy_path?, probed_info? }`.
- **Tracks** — ordered list of clip lanes, video and (future) audio.
- **Clips** — references into a media source with `(source_in, source_out)` and a `timeline_position`.
- **Time mapping** — given a `timeline_time`, what clip is active on each track and what `media_time` does that translate to.
- **Command history** — every edit is a `Command` applied via the command pattern; full **undo / redo**.
- **Schema versioning** — every serialized project carries a version; load handles migration.
- *(Future)* effects on clips.
- *(Future)* transitions between clips.
- *(Future)* audio tracks and clip audio.

**Timeline does NOT own:**

- The **engine.** Timeline computes `(source_id, media_time)` and hands it to whoever's calling. It doesn’t talk to engine directly.
- The **renderer.** Timeline doesn't know about pixels.
- The **UI.** Slint is a consumer; timeline is pure data + logic.
- **File I/O for media.** Timeline holds paths; the engine opens files.

**Stance:** **library crate, no I/O, no async, no threads.** Pure data + transforms. The same testability vibe as decoder/engine/renderer — headless, deterministic, hammered with unit tests.

---

## Core types (sketch — names TBD)

```rust
pub struct Project {
    pub schema_version: u32,
    pub id: ProjectId,
    pub settings: ProjectSettings,         // frame rate, canvas size, etc.
    pub sources: HashMap<MediaSourceId, MediaSource>,
    pub tracks: Vec<Track>,
    pub history: History,                   // undo/redo
}

pub struct MediaSource {
    pub id: MediaSourceId,
    pub original_path: PathBuf,
    pub proxy_path: Option<PathBuf>,        // populated by future proxy pipeline
    pub probed: Option<ProbedInfo>,         // dimensions, duration, etc., filled in async by engine probe
}

pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,                    // Video | Audio (audio is future)
    pub clips: Vec<Clip>,                   // sorted by timeline_position, non-overlapping
    pub muted: bool,
    pub locked: bool,
}

pub struct Clip {
    pub id: ClipId,
    pub source_id: MediaSourceId,
    pub source_in: Rational,                // start within source media
    pub source_out: Rational,               // end within source media
    pub timeline_position: Rational,        // where the clip starts on the timeline
    // Future: effects, transform, speed, etc.
}
```

**Key invariants** (enforced by `Project` operations, not by struct construction):

- Clips on a track are **sorted by `timeline_position`** and **non-overlapping**.
- `source_in < source_out`.
- A clip's `(source_in, source_out)` range fits within the source's known duration (when probed; before probe, trust the user).
- `MediaSourceId`s referenced by clips exist in `sources`.

---

## Rational time, end to end

Same `Rational` as decoder / engine. **Floats never enter the model.**

- Frame rates are `Rational` (24/1, 30000/1001, 60/1, etc.).
- Clip positions, in-points, out-points: `Rational`.
- Project duration: derived from clip positions; `Rational`.

`f64` is for UI display only.

---

## Time mapping (the core algorithm)

Given a **`timeline_time`** and a **`TrackId`**, return what clip is active and what **`media_time`** in its source corresponds:

```rust
pub struct ActiveClip {
    pub clip_id: ClipId,
    pub source_id: MediaSourceId,
    pub media_time: Rational,
}

impl Project {
    pub fn active_clip_on_track(&self, track: TrackId, t: Rational) -> Option<ActiveClip>;
}
```

**Algorithm:**

1. Find track.
2. Binary search clips by `timeline_position` for the clip whose range `[pos, pos + (out - in))` contains `t`.
3. If found: `media_time = source_in + (t - timeline_position)`.

**Time complexity:** O(log N) per track per query. With typical edit sizes (hundreds to low thousands of clips), this is invisible.

**Multi-track queries** (future): `active_clips_at(t) -> Vec<ActiveClip>` returns active clip per track, in track order (topmost-first for compositing).

---

## Command pattern + undo / redo

Every edit is a **`Command`**. Commands are:

- **Pure functions** over project state: `fn apply(&self, &mut Project)`.
- **Reversible**: each command records enough state to undo itself.
- **Atomic**: half-applied commands are a bug, not a feature.

```rust
pub trait Command: Send {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError>;
    fn undo(&mut self, project: &mut Project);
    fn label(&self) -> &str;                // for UI display in history
}

pub struct History {
    undo_stack: Vec<Box<dyn Command>>,
    redo_stack: Vec<Box<dyn Command>>,
    max_depth: usize,                       // bounded history
}
```

**MVP commands:**

- `AddSource { path }` → adds a `MediaSource`; reverse drops it (and any clips referencing it — error if any exist).
- `RemoveSource { source_id }` → only if no clips reference; otherwise `TimelineError::SourceInUse`.
- `AddClip { track_id, clip }` → inserts clip; reverse removes by clip id.
- `RemoveClip { clip_id }` → removes; reverse re-inserts at original position.
- `MoveClip { clip_id, new_position }` → shifts on timeline; reverse restores old position.
- `TrimClipIn { clip_id, new_in }` / `TrimClipOut { clip_id, new_out }` → adjust source bounds; reverses restore.
- `AddTrack { kind }` / `RemoveTrack { track_id }` → reversible track ops.

**Future commands:**

- `SplitClip { clip_id, at_time }`
- `AddEffect { clip_id, effect }`
- `ApplyTransition { clip_a, clip_b, transition }`
- `ChangeClipSpeed { clip_id, factor }`

**Coalescing** (future): rapid drag operations (e.g. trimming) should coalesce into one history entry. MVP doesn't bother — every command pushes one entry.

---

## Validation: commands fail loudly

Every command's `apply` returns `Result`. **No silent fixups.** Invalid edits (overlapping clips, out-of-range trims, missing source IDs) return `TimelineError`.

```rust
pub enum TimelineError {
    TrackNotFound(TrackId),
    ClipNotFound(ClipId),
    SourceNotFound(MediaSourceId),
    SourceInUse { source_id: MediaSourceId, by_clips: Vec<ClipId> },
    ClipOverlap { existing: ClipId, attempted_position: Rational },
    InvalidTrim { reason: &'static str },
    SchemaUnsupported { found: u32, supported_max: u32 },
    Serde(String),
}
```

UI is responsible for preventing invalid commands in normal user flow (e.g. don't show drop targets that would overlap). Errors are the **safety net**, not the **error UX**.

---

## Serialization

**Format: JSON** for MVP. Human-readable, debuggable, every editor has tooling for it. Faster binary formats (CBOR, postcard) later if profile shows it matters.

**Schema versioning** is from day one (project rule, not optional):

```rust
#[derive(Serialize, Deserialize)]
pub struct Project {
    pub schema_version: u32,             // bumped on breaking change
    // ...
}

pub fn load_project(json: &str) -> Result<Project, TimelineError> {
    let v: ProjectVersionProbe = serde_json::from_str(json)?;
    match v.schema_version {
        1 => serde_json::from_str(json).map_err(...),
        // Future: 2 => migrate_v1_to_v2(...),
        v if v > CURRENT_SCHEMA => Err(TimelineError::SchemaUnsupported { ... }),
        _ => Err(...),
    }
}
```

**No file I/O in the timeline crate.** `serialize(&Project) -> String` and `deserialize(&str) -> Result<Project>`. Caller does file writes. Keeps timeline pure and testable.

---

## IDs

`MediaSourceId(u64)`, `ClipId(u64)`, `TrackId(u64)`, `ProjectId(Uuid)`.

**IDs are stable across saves.** They're allocated from a counter held in `Project` (or `History`). Don't reuse on remove — gaps are fine.

**Why u64 not Uuid for internal IDs:** UUIDs bloat JSON output, are noisy in logs, and offer nothing over a project-scoped counter inside a single project file. Cross-project references would need UUIDs — out of scope.

`ProjectId` is `Uuid` because it crosses files / users / machines.

---

## Engine integration boundary

Timeline doesn't import engine. Timeline doesn't know engine exists. The **app / consumer layer** wires them:

```rust
// In app code, not timeline:
let timeline_t = playhead_position;
let active = project.active_clip_on_track(video_track, timeline_t);
if let Some(a) = active {
    engine.seek_exact(a.source_id, a.media_time);
}
```

This is the contract from `engine-research.md` finally cashed in: **engine speaks `(SourceId, media_time)`, timeline produces those, consumer wires the call.**

**Source ID mapping:** timeline's `MediaSourceId` maps 1:1 to engine's `SourceId`. The app keeps a small map `MediaSourceId → engine::SourceId` populated when sources are opened in the engine. Could be the same `u64` under the hood — TBD as an app concern.

---

## Probed metadata flow

Timeline has `MediaSource.probed: Option<ProbedInfo>`. When the app opens a source in the engine and gets an `Opened` event with `SourceInfo`, it dispatches a `SetSourceProbed { source_id, info }` command into the timeline. This updates the timeline's view and pushes a history entry (or doesn't — probe data isn't really "undoable"; flag this and consider whether some commands skip history).

**Design note:** distinguish **user edits** (history-pushed) from **system updates** (probe results, file moves, etc. — applied but not undoable). MVP: probe is system, doesn't push history. All other MVP commands push history.

---

## Frame rate and snapping *(future, post-MVP)*

A real editor snaps clip edits to frame boundaries based on the project's frame rate. MVP doesn't snap — `Rational` times can be anything. v1.1 introduces:

- `ProjectSettings.frame_rate: Rational`
- Snap-to-frame option per command.
- Frame-quantized rational arithmetic helpers.

For MVP, the UI can pass already-snapped values if it wants snapping behavior.

---

## Effects, transitions, speed *(future)*

When effects exist:

- `Clip.effects: Vec<Effect>` — an ordered chain.
- Commands: `AddEffect`, `RemoveEffect`, `ReorderEffects`.
- Renderer extension: per-layer effect chain (see renderer-research.md Phase 13).

Transitions and speed are similar — additive data on `Clip` plus their command set. **Architecturally cheap**, just adds work.

---

## Audio tracks *(future)*

When the audio decoder exists:

- `TrackKind::Audio` becomes real (not just an enum variant placeholder).
- `Clip` on audio tracks holds the same `(source_in, source_out, timeline_position)`.
- Volume, pan, fade in/out as clip properties.
- A separate `audio_engine` crate consumes timeline's audio clip queries.

---

## Concurrency / threading

**`Project` is not thread-safe.** All edits happen on one thread (the UI thread, in practice). Background tasks (probe results, render thumbnails, etc.) communicate via the **app's** command channel, not by mutating the project directly.

`Project` is `Send` but not `Sync`. Caller can move it between threads but not share. **MVP keeps it simple:** project lives on UI thread, all commands applied synchronously.

---

## Testability

Same vibe as engine and decoder — **every meaningful behavior gets a unit test**.

- Time mapping: build a project with known clips, query at various `t`, assert correct `ActiveClip`.
- Commands: apply + undo round-trip leaves project byte-identical to start (via serialization equality).
- Invariants: ops that would violate sorting / overlap rules return errors, project state unchanged.
- Serialization: round-trip a project through JSON, deep-equal check.
- Schema versioning: load a v1 project under v1 reader → works; under v2 reader → migrates or errors per design.

---

## MVP scope (what we build today)

1. **`Project` root** with `schema_version`, `id`, `settings`, `sources`, `tracks`, `history`.
2. **`MediaSource`**, **`Track`** (Video kind only), **`Clip`** types.
3. **Time mapping** — `active_clip_on_track(track, t) -> Option<ActiveClip>`.
4. **Command pattern + undo/redo** — `Command` trait, `History` bounded stack.
5. **MVP command set**: `AddSource`, `RemoveSource`, `AddClip`, `RemoveClip`, `MoveClip`, `TrimClipIn`, `TrimClipOut`, `AddTrack`, `RemoveTrack`, `SetSourceProbed` (system, no history).
6. **JSON serialization** with schema versioning.
7. **`TimelineError`** with concrete variants.
8. **`Send`-not-`Sync` `Project`**, no internal threading.

## Out of scope today (documented, deferred)

- Audio tracks (placeholder enum variant only)
- Effects, transitions, speed changes
- Frame snapping
- Command coalescing for drag operations
- Multi-track compositing queries (`active_clips_at`)
- Binary serialization
- Cross-project / asset library
- Collaborative editing / CRDT

---

## Known limits (MVP)

- **Single project at a time** — no project switching, no multi-project state.
- **All edits push history** — no distinction between “user edit” and “tweak” (other than probe).
- **No snapping** — clip positions are arbitrary `Rational`.
- **No coalescing** — dragging a clip 100 pixels = 100 history entries (MVP doesn’t care; v1.1 fixes).
- **JSON-only** — no binary format yet.
- **Single-threaded `Project`** — UI thread owns it.