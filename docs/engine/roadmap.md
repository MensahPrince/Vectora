# Engine roadmap

Implementation plan for the **`engine`** crate. Follows the design in **`research.md`** (same folder). Build in this order — each phase ends in something **runnable and tested**.

**Scope discipline:** everything before the **MVP cutline** ships in v1. Everything after is **documented but deferred** — design rationale already exists in `research.md`; the phases here are the *implementation* plan when their time comes.

## Critical review (implementation notes)

These showed up while landing the MVP; they are worth keeping next to the phased plan:

- **`Engine::Drop` field order:** Rust drops struct fields in **declaration order** (first field first). `WorkerJoin` (or any “join the worker” guard) must be declared **after** the `crossbeam_channel::Sender` fields on `Engine`, otherwise the worker is joined while `recv` is still waiting on a live sender and shutdown **deadlocks**.
- **Scrub vs command ordering:** `crossbeam_channel::select!` does not promise fairness across channels in a useful way for UI. The scrub branch drains pending **commands** with `try_recv` before applying the scrub slot so `seek_exact` + `seek_scrub` in the same tick do not get reordered with scrub first.
- **Filenames:** Older drafts used the name `engine-research.md`; the checked-in doc is **`research.md`** in this directory.

---

## Phase 0 — Workspace scaffold

**Goal:** the `engine` crate exists, depends on `decoder`, and `cargo build -p engine` succeeds.

**Tasks:**

1. Create `crates/engine` (lib) in the workspace.
2. Add deps to `crates/engine/Cargo.toml`:
   - `decoder = { path = "../decoder" }`
   - `crossbeam-channel` (for `Sender` / `Receiver` / `select!`)
   - `thiserror`
3. Re-export the decoder types the engine API surfaces: `Rational`, `DecodedVideoFrame`, `SourceInfo`, `DecoderError`. Consumers shouldn’t need a direct `decoder` dep just to match on errors.

**Deliverable:** `cargo build -p engine` clean. No public API yet beyond `pub use decoder::{...}`.

---

## Phase 1 — Core types

**Goal:** all engine-facing types compile and have unit tests. No threading, no decoder calls.

**Modules:**

- `ids` — `SourceId(u64)`, `RequestId(u64)`, both `Copy + Eq + Hash + Display`.
- `command` — `EngineCommand` enum (`Open`, `SeekExact`, `NextFrame`, `Close`). **No `SeekScrub` variant** — scrub goes through a separate path.
- `event` — `EngineEvent` enum (`Opened`, `Frame`, `Eof`, `Error`, `Closed`) with optional `RequestId` per variant.
- `error` — `EngineError` enum via `thiserror`, with `#[from] decoder::DecoderError`.

**Tests:**

- Hash / equality on `SourceId` and `RequestId`.
- `EngineError::Display` produces a useful string for each variant.
- Round-trip a `Frame` event through a crossbeam `unbounded()` channel — proves the types are `Send`.

**Deliverable:** `cargo test -p engine` passes with type-only unit tests.

---

## Phase 2 — Worker thread skeleton + `Open`

**Goal:** spawn a worker thread, handle the `Open` command, emit `Opened` or `Error`, exit cleanly on disconnect.

**Tasks:**

1. `pub struct Engine` holding:
   - `tx_cmd: Sender<EngineCommand>`
   - `tx_scrub_signal: Sender<()>` *(unused until Phase 4, wire now)*
   - `scrub_slot: Arc<Mutex<Option<(SourceId, Rational)>>>` *(unused until Phase 4, wire now)*
   - `request_counter: Arc<AtomicU64>`
   - `worker: Option<JoinHandle<()>>` *(taken in `Drop` for join)*
2. `pub struct EventReceiver(Receiver<EngineEvent>)` with `recv` / `try_recv` / `recv_timeout` helpers.
3. `pub fn Engine::new() -> (Engine, EventReceiver)`:
   - Create `bounded(16)` cmd channel, `bounded(4)` event channel, `bounded(1)` scrub signal.
   - Spawn the worker thread; move all receivers + the event sender into it.
4. `worker_loop`:
   - Holds `HashMap<SourceId, Decoder>` (capacity 1 in MVP, but the type is right from day one).
   - Loops on `select!` over `rx_cmd` and `rx_scrub_signal` *(signal branch is a stub returning until Phase 4)*.
   - On `EngineCommand::Open`: call `Decoder::open(&path)`; on `Ok` insert into map and send `Opened`; on `Err` send `Error`.
   - On `rx_cmd` disconnect: `break` cleanly.
5. `impl Drop for Engine`: drop `tx_cmd`, join the worker thread.

**Tests (integration in `tests/engine_integration.rs`):**

- `engine_opens_real_file`: `Engine::new()`, send `Open` for `testsrc_h264.mp4`, receive `Opened` event with expected `width` / `height` / `pixel_format`.
- `engine_open_missing_file_emits_error`: `Open` a bad path → receive `Error { error: Decoder(DecoderError::Open(_)), .. }`.
- `engine_drop_joins_worker`: drop the `Engine`, the worker thread terminates (proved by event channel disconnecting on the consumer side within a reasonable timeout).

**Deliverable:** engine opens files end-to-end via the channel API. Worker shutdown is clean.

---

## Phase 3 — Frame pump (`SeekExact`, `NextFrame`, `Close`)

**Goal:** the engine drives the decoder through seeks and frame pumps, emits `Frame` / `Eof` events with correct `RequestId` correlation.

**Tasks:**

1. Handle `EngineCommand::SeekExact { source_id, target, request_id }`:
   - Look up decoder; if missing → `Error::SourceNotFound`.
   - Call `decoder.seek_exact(target)`.
   - On `Ok(DecodeOutcome::Frame(f))` → `Event::Frame { request_id: Some(_) }`.
   - On `Ok(DecodeOutcome::Eof)` → `Event::Eof { request_id: Some(_) }`.
   - On `Err` → `Event::Error`.
2. Handle `EngineCommand::NextFrame { ... }` similarly.
3. Handle `EngineCommand::Close { source_id }`:
   - Remove from map, send `Event::Closed`.
   - If source missing, ignore silently (idempotent close).
4. Engine handle methods:
   - `fn open(&self, path: PathBuf) -> (SourceId, RequestId)` — assigns next IDs, sends `Open`.
   - `fn seek_exact(&self, source_id: SourceId, target: Rational) -> RequestId`.
   - `fn next_frame(&self, source_id: SourceId) -> RequestId`.
   - `fn close(&self, source_id: SourceId)`.
   - Methods are infallible at the API level — failures arrive as `Event::Error`. If `tx_cmd.send` fails (worker dead), panic *or* return a `WorkerDead` error from a separate `try_*` variant — pick one and document. **Recommendation: panic on send failure**, since worker death is unrecoverable; `try_send` API can be added later if needed.

**Tests:**

- `engine_seek_exact_to_two_seconds`: open, seek to 2s, receive `Frame` with `pts ≈ 2.0s` and matching `request_id`.
- `engine_seek_exact_bframes`: same on `testsrc_bframes.mp4`.
- `engine_next_frame_after_seek_monotonic`: seek to 2s, then 3 × `next_frame`, PTSes strictly increasing.
- `engine_close_releases_source`: open, close, `next_frame` on closed `SourceId` → `Error::SourceNotFound`.
- `engine_seek_past_eof`: receive `Eof` event with the request_id.

**Deliverable:** engine drives `seek_exact` / `next_frame` / `close` end-to-end. Correlation via `RequestId` works.

---

## Phase 4 — Scrub coalescing

**Goal:** rapid `seek_scrub` calls coalesce to latest-wins. Worker is responsive.

**Tasks:**

1. Engine handle:
   ```rust
   pub fn seek_scrub(&self, source_id: SourceId, target: Rational) {
       *self.scrub_slot.lock().unwrap() = Some((source_id, target));
       let _ = self.tx_scrub_signal.try_send(()); // OK if already pending
   }
   ```
2. Worker `select!` scrub branch:
   ```rust
   recv(rx_scrub_signal) -> _ => {
       let target = scrub_slot.lock().unwrap().take();
       if let Some((sid, t)) = target {
           match decoders.get_mut(&sid) {
               Some(dec) => match dec.seek_scrub(t) {
                   Ok(DecodeOutcome::Frame(f)) => send(Event::Frame { request_id: None, .. }),
                   Ok(DecodeOutcome::Eof)     => send(Event::Eof   { request_id: None, .. }),
                   Err(e)                     => send(Event::Error { error: e.into(), .. }),
               },
               None => send(Event::Error { error: SourceNotFound(sid), .. }),
           }
       }
   }
   ```
3. **Cross-operation behavior** — verify by test:
   - Scrub during in-flight `SeekExact` (same source) → exact runs to completion, scrub processed on next iteration.
   - Two scrubs back-to-back → only the latest is acted upon (or the first is, then the second — both acceptable; what matters is **stale ones never run**).

**Tests:**

- `scrub_coalesces_burst`: fire 20 `seek_scrub` calls in a tight loop with monotonically increasing targets. Receive **at most 20** `Frame` events with `request_id: None`. The **last** delivered frame’s PTS corresponds to (one of) the most recent scrub targets. Older targets may or may not have been acted on — they MUST NOT all have been.
- `scrub_after_exact_does_not_lose_exact`: send `seek_exact(2.0s)` then immediately `seek_scrub(4.0s)`. Receive at least one frame for the exact (request_id `Some(_)`, PTS ≈ 2.0s) and one for the scrub (request_id `None`, snap near 4.0s).
- `scrub_on_unopen_source`: emit `Error::SourceNotFound` event.

**Deliverable:** scrub coalescing works under burst. Cross-operation interactions don’t lose committed seeks.

---

## Phase 5 — Headless test harness binary

**Goal:** a runnable example that exercises the full MVP engine API end-to-end. Replaces the decoder’s `dump_frames` as the primary smoke test.

**Tasks:**

1. `examples/playground.rs`:
   - Args: `<path> [--script <path>]`.
   - Default script: open → seek_exact(0) → next_frame × 5 → seek_scrub(2.5) → seek_exact(2.0) → next_frame × 3 → close.
   - Spawn a consumer thread that drains events and prints `[event] source=… request=… ...`.
   - Main thread submits commands per script with small sleeps between them.
2. Optional: `--script` reads a tiny DSL (one command per line, e.g. `seek_exact 2/1`, `next 5`, `scrub 5/2`) for ad-hoc testing.

**Deliverable:** `cargo run -p engine --example playground -- crates/engine/tests/assets/testsrc_h264.mp4` runs the default script and prints sensible events.

---

## Phase 6 — Polish: errors, docs, README, clippy

**Goal:** ship-quality library hygiene.

**Tasks:**

1. Doc comments on every public item:
   - `Engine` — threading note, drop semantics, channel sizes.
   - `EventReceiver` — single-consumer contract.
   - `seek_scrub` vs `seek_exact` — semantics, `RequestId` presence, scrub coalescing contract.
   - `EngineEvent` variants — when each fires and what `request_id` means.
2. `crates/engine/README.md`: one-screen overview + quickstart + link to `research.md`.
3. `cargo clippy -p engine --all-targets -- -D warnings` clean.
4. `cargo doc -p engine --no-deps --open` renders coherently.
5. Audit every `?` and `.unwrap()` outside tests — replace with the right `EngineError` variant or document why the unwrap is sound.

**Deliverable:** all green: `cargo test -p engine`, `cargo clippy -p engine`, `cargo doc -p engine`.

---

## 🚧 MVP cutline — engine ships here

Everything below is **documented** in `research.md`. Implementation comes **after** the renderer / UI integration proves the MVP works in anger. Don’t pre-build them — premature optimization gives you a junk drawer instead of a tool.

---

## Phase 7 — Decoder pool + LRU *(post-MVP)*

**Goal:** multiple sources open concurrently; `max_concurrent_decoders` cap with LRU eviction.

**Tasks:**

1. `EngineConfig { max_decoders: usize }`, default 4.
2. Worker holds `HashMap<SourceId, Decoder>` + `VecDeque<SourceId>` (LRU order).
3. On `Open` at capacity → evict oldest, emit `Closed` event for evicted source.
4. On any operation against a source → bump to MRU.
5. New error: `EngineError::PoolEvicted { source_id }` if the engine somehow uses an evicted source ID (shouldn’t happen; defensive).

**Tests:**

- Open `max + 1` sources → oldest evicted, `Closed` event emitted.
- Using an evicted `SourceId` → `Error::SourceNotFound`.
- LRU recency: open A, open B, use A, open C, open D → B evicted (not A).

---

## Phase 8 — Decoded frame cache *(post-MVP)*

**Goal:** repeated scrubs / seeks over the same range don’t re-decode.

**Tasks:**

1. `FrameCache` keyed by `(SourceId, Rational pts)`, evicted by **total bytes** (not entry count — a 4K YUV frame is ~12 MB).
2. Configurable byte budget, default e.g. 256 MB.
3. Inserted on every successful decode result.
4. Looked up before submitting a `SeekExact` / `NextFrame` to the decoder.
5. Invalidated on `Close` for that source.

**Tests:**

- Same `seek_exact` twice in a row → second one hits cache (assert via a debug counter or `tracing` span).
- Cache eviction under budget pressure works (insert until eviction triggers).

---

## Phase 9 — Playback clock *(post-MVP)*

**Goal:** real-time playback driven by an internal clock; drift handling.

**Tasks:**

1. `EngineCommand::Play { source_id, rate: f32 }` and `Pause { source_id }`.
2. A clock thread (or timer in the worker) schedules `NextFrame`-equivalent decodes at the source frame rate × rate.
3. Drift detection: if decode lags > N frame intervals → emit `Event::Lag { source_id, frames_behind }`. Consumer can switch to proxy or accept skips.
4. Pause holds the clock; resume continues from current PTS.

---

## Phase 10 — Proxy / original path selection *(post-MVP)*

**Goal:** transparently choose proxy or original per source.

**Tasks:**

1. `Open` accepts `{ original: PathBuf, proxy: Option<PathBuf> }`.
2. `Engine` has a global `use_proxies: AtomicBool`.
3. Switching modes mid-session emits `Closed` for the affected source(s); consumer re-opens.

---

## Phase 11 — Slint integration *(post-MVP)*

**Goal:** wire the engine to the actual Cutlass UI preview pane.

**Tasks:**

1. Bridge layer (probably in the `app` crate, not engine):
   - Slint preview component holds an `EventReceiver`.
   - On Slint tick, drain events, hand the latest `Frame` to the renderer for wgpu upload.
   - Slint scrubber drag → `engine.seek_scrub(...)`. Scrubber release → `engine.seek_exact(...)`.
2. Performance pass: confirm scrub latency feels smooth (< 1 frame budget) on `testsrc_h264.mp4` and a real 4K source.

---

## Test asset reference

Engine integration tests reuse the **decoder’s** test fixtures via a copy or symlink into `crates/engine/tests/assets/`. **Recommendation:** include a `regenerate.sh` mirroring the decoder’s — engine tests should be independently regenerable, not reach into a sibling crate’s assets directory.

Assets needed for MVP:

| File | Used in phase | Purpose |
|---|---|---|
| `testsrc_h264.mp4` | 2, 3, 4, 5 | Basic open + seek + scrub. |
| `testsrc_bframes.mp4` | 3 | Confirms B-frame seek correctness survives the engine layer. |
| `audio_only.m4a` | 2 | Confirms `Error::Decoder(DecoderError::Unsupported)` bubbles cleanly. |

---

## Order-of-operations rule

**Don’t parallelize phases.** Phase 4 (scrub coalescing) depends on Phase 3 (frame pump) being deterministic — if `seek_exact` produces wrong PTSes, you’ll waste hours blaming the coalescing logic. Each phase’s tests must be **green** before starting the next one. Same rule as decoder.

## Out of scope for engine *(forever, or until proven needed)*

- Audio decode and A/V sync (different crate, much later).
- Filter graph / compositing (renderer territory; engine just delivers frames).
- Network streaming sources (decoder can technically handle URLs; not a goal).
- Multi-process / IPC engine (the whole point is in-process for low latency).