# Engine research

The **engine** crate owns `Decoder` instances and turns *“give me frame at media time T from source X”* requests into decoded frames delivered on a channel. It is the **scheduling and lifecycle** layer between the library-only **`decoder`** crate and any consumer (renderer, UI, headless harness).

This doc covers the **full vision**. The **MVP scope** is called out explicitly at the bottom and again per-section where the line falls.

---

## Mission and boundaries

**Engine owns:**

- One or more `Decoder` instances (one in MVP, pool later).
- One decode worker thread (per pool; multi-worker is future-future).
- Command and event channels (crossbeam, runtime-agnostic).
- Scrub coalescing.
- *(Future)* decoder pool with LRU eviction.
- *(Future)* decoded frame cache.
- *(Future)* playback clock.
- *(Future)* proxy / original path selection.

**Engine does NOT own:**

- The **timeline / clip model.** Consumers map `clip → (source_id, media_time)` and submit those to engine. Baking the timeline into engine is a one-way door — don’t.
- The **renderer.** Engine outputs `DecodedVideoFrame` on a channel; renderer uploads to wgpu.
- The **UI.** Slint is a *consumer* of engine; not the other way around.
- **Audio.** Out of scope entirely until the audio decoder exists.

**Async stance:** engine is **sync + crossbeam channels**, runtime-agnostic. Consumers that want async wrap channels in their own runtime (cheap). Tokio is not a dependency.

---

## Worker thread model

`Decoder` is **`!Sync` and thread-confined** (see `decoder-research.md`). Engine wraps it with **one decode worker thread**. The worker:

- Owns the `Decoder` instance(s).
- Receives commands on a crossbeam channel.
- Sends events on another crossbeam channel.
- Reads the scrub slot when signalled (see below).
- Exits cleanly when the command channel disconnects (engine handle dropped).

```rust
// Sketch (names TBD).
fn worker_loop(
    rx_cmd: Receiver<EngineCommand>,
    rx_scrub_signal: Receiver<()>,
    scrub_slot: Arc<Mutex<Option<(SourceId, Rational)>>>,
    tx_event: Sender<EngineEvent>,
) {
    let mut decoders: HashMap<SourceId, Decoder> = HashMap::new(); // size 1 in MVP
    loop {
        select! {
            recv(rx_cmd) -> msg => match msg {
                Ok(cmd) => handle_command(cmd, &mut decoders, &tx_event),
                Err(_) => break, // engine dropped
            },
            recv(rx_scrub_signal) -> _ => {
                if let Some((sid, target)) = scrub_slot.lock().unwrap().take() {
                    handle_scrub(sid, target, &mut decoders, &tx_event);
                }
            }
        }
    }
}
```

---

## Command / event channel design

Commands flow IN, events flow OUT. **Both are owned types** — no borrows across the thread boundary.

```rust
// Commands (sketch).
pub enum EngineCommand {
    Open       { source_id: SourceId, path: PathBuf, request_id: RequestId },
    SeekExact  { source_id: SourceId, target: Rational, request_id: RequestId },
    NextFrame  { source_id: SourceId, request_id: RequestId },
    Close      { source_id: SourceId },
    // SeekScrub does NOT live here — see "Scrub coalescing".
}

// Events (sketch).
pub enum EngineEvent {
    Opened { source_id, info: SourceInfo, request_id: RequestId },
    Frame  { source_id, frame: DecodedVideoFrame, request_id: Option<RequestId> },
    Eof    { source_id, request_id: Option<RequestId> },
    Error  { source_id: Option<SourceId>, error: EngineError, request_id: Option<RequestId> },
    Closed { source_id },
}
```

### Request IDs

Monotonic `RequestId(u64)` minted by the engine handle. Without it, a consumer can’t distinguish *“the frame from my exact seek”* from *“some other frame that arrived first.”*

- `Open`, `SeekExact`, `NextFrame` carry a `RequestId`; the matching `Opened` / `Frame` / `Eof` event echoes it.
- **`SeekScrub` does NOT carry a `RequestId`** — scrub is fire-and-forget-latest by design. `Frame` events from scrubs carry `request_id: None`.

---

## Scrub coalescing: latest-wins slot

The decoder doc **contracts** that scrub seeks are cheap (O(GOP)) and the **engine** drops stale targets. Implementation uses a **separate slot + signal** rather than a regular command:

- `scrub_slot: Arc<Mutex<Option<(SourceId, Rational)>>>` — the latest scrub target.
- `tx_scrub_signal: Sender<()>` (`bounded(1)`) — wakes the worker.

```rust
// Engine handle.
pub fn seek_scrub(&self, source_id: SourceId, target: Rational) {
    *self.scrub_slot.lock().unwrap() = Some((source_id, target));
    let _ = self.tx_scrub_signal.try_send(()); // OK if full (signal already pending)
}
```

**Why this works:**

- Multiple `seek_scrub` calls in quick succession **overwrite** the slot. By the time the worker drains, only the latest target remains.
- Signal channel being full means a wake-up is already pending — `try_send` failure is harmless.
- Scrubs do **not** carry `RequestId` — they’re inherently coalesced.

**Cross-operation interaction (same source):**

- Scrub arrives while `SeekExact` mid-flight → exact runs to completion (decoder is sync, no interruption), then on the next worker iteration the scrub slot is consumed.
- Scrub arrives while another scrub mid-flight → in-flight scrub finishes (cheap), latest slot value runs next.

**Alternative rejected:** sending scrubs through the regular command channel and draining-to-latest on arrival. Works but reorders work relative to non-scrub commands and complicates the worker loop. The slot is simpler and the contract is more obvious.

---

## Source IDs

`SourceId(u64)` — opaque, monotonically assigned by engine on `open` (or supplied by caller; TBD).

**MVP has exactly one source.** `SourceId` is still in the API because:

- Zero cost (`u64` newtype).
- The whole point of doing engine “properly” is the pool drops in without an API break.
- Saves an awkward refactor where every command grows a parameter.

---

## Backpressure and channel sizing

**MVP channel sizes (tunable):**

| Channel | Size | Rationale |
|---|---|---|
| Command | `bounded(16)` | Absorbs bursts; submitters block if overrun. |
| Event | `bounded(4)` | Consumers should drain promptly; small to keep latency low. |
| Scrub signal | `bounded(1)` | Wake-up only, full == already pending. |

**Drain contract:** consumers MUST pull events or the worker will block on `send`. Document this on the engine handle.

**Future consideration (v1.1):** replace the event channel with a **latest-only frame slot** for previews — the editor only ever wants the *latest* frame, older ones are dead on arrival. Mechanism is the same pattern as the scrub slot. Optimization, not architecture.

---

## Error model

```rust
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("source not found: {0:?}")]
    SourceNotFound(SourceId),

    #[error("source already open: {0:?}")]
    SourceAlreadyOpen(SourceId),

    #[error("decoder failure")]
    Decoder(#[from] decoder::DecoderError),

    #[error("worker thread is dead — engine handle is invalid")]
    WorkerDead,
}
```

`DecoderError` (which already preserves `ffmpeg::Error`) bubbles up wrapped; engine adds its own variants for engine-specific failures. Consumers match the variant to decide retry / fall-back policy.

---

## Threading & Send/Sync (engine surface)

- **`Engine` handle:** `Clone + Send + Sync`. Internally an `Arc` over `{ tx_cmd, tx_scrub_signal, scrub_slot, request_id_counter }`. Multiple consumers can submit commands; the worker serializes.
- **`EventReceiver`:** `Send`, **not cloned**. One consumer (or a user-built dispatcher). MVP assumes single consumer.

This lets a UI thread and a background preload task both submit commands without engine-level locking beyond what crossbeam already does.

---

## Shutdown semantics

**Drop-based.** When the last `Engine` handle drops:

1. `tx_cmd` drops → worker `recv` returns `Err`.
2. Worker exits its `select!` loop.
3. The `JoinHandle` held by `Engine`’s `Drop` impl joins the worker thread.

**In-flight work:** any command the worker had already pulled before shutdown runs to completion. The matching event may or may not reach a consumer depending on the event receiver state. **Document this** — don’t rely on receiving a final `Closed` event.

**No explicit `Shutdown` command needed.** Could add one for cleaner test teardown later; not in MVP.

---

## Pool architecture *(future, post-MVP)*

When scaling beyond one decoder:

- Worker owns `HashMap<SourceId, Decoder>` plus an LRU order (e.g. `LinkedHashMap` or a `VecDeque<SourceId>` companion).
- `max_concurrent_decoders` config. On `Open` at capacity → evict LRU entry, send `Closed` event for the evicted source.
- Same-path reopen is the **caller’s** decision (via `SourceId`); engine doesn’t dedupe by path.
- Pool stays **on the same single worker thread**. Multiple workers (one-per-decoder, or a thread-pool with a scheduler) is **future-future** — changes pump strategy, not the API.

---

## Frame cache *(future, post-MVP)*

Decoded frames are expensive. An LRU cache keyed by `(SourceId, Rational)` evicted by total memory cost can serve repeated scrubs over the same range without re-decoding.

- **Optimization, not architecture** — sits between worker and `tx_event`.
- Eviction policy by **bytes**, not entry count (a 4K YUV frame is ~12 MB; entry count is meaningless).
- Cache invalidation on `Close` is straightforward (drop all entries for that source). On `Open` of the same path under a new `SourceId`, treat as new — no cross-source sharing.

---

## Playback clock *(future, post-MVP)*

For real-time playback:

- A clock schedules `NextFrame` requests at the source frame rate (or any user-chosen rate).
- Drift detection: if decode lag exceeds one frame interval consistently, fall back to frame-skip or proxy.
- Skip-ahead policy when decoder can’t keep up.

**MVP has no clock.** Frames are pulled on demand. A test harness or UI animation tick drives `NextFrame` externally.

---

## Proxy resolution *(future, post-MVP)*

Engine accepts `{ original: PathBuf, proxy: Option<PathBuf> }` per source plus a global “use proxies” flag. Engine picks the path; decoder is path-agnostic.

**Zero impact on decoder** — purely an engine concern. Switching proxy ↔ original mid-session requires Close + Open at the new path (different decoder state).

---

## Consumer integration

**MVP target: headless test harness.** A binary (`examples/playground.rs` or similar) that:

- Opens a source via the engine handle.
- Drives a scripted sequence: open → scrub → scrub → scrub → seek_exact → next_frame × N.
- Prints events with PTS and timing.
- Serves as the smoke test until the renderer / UI exist.

**Future: Slint preview pane.** Same engine API, different consumer. UI thread submits commands, polls events on the main loop tick, hands frames to the renderer for wgpu upload.

---

## MVP scope (what we are doing today)

1. **One `Decoder` instance**, behind one worker thread.
2. **Crossbeam channels** for command and event.
3. **Scrub coalescing** via latest-wins slot + signal.
4. **`SourceId` + `RequestId`** in the API from day one (single source in MVP).
5. **Headless test harness** as the integration target.
6. **`EngineError`** wrapping `DecoderError` with engine-specific variants.
7. **Drop-based shutdown**, no explicit `Shutdown` command.

## Out of scope today (documented, deferred)

- Decoder pool / LRU eviction
- Decoded frame cache
- Playback clock and frame scheduling
- Proxy / original path selection
- Slint integration (UI consumer)
- Multiple worker threads
- Audio of any kind
- Async API surface (consumers wrap channels themselves if needed)
- `Shutdown` command (drop is enough)

---

## Known limits (MVP)

These are deliberate. Documented so future-us knows they were chosen, not missed.

- **One source at a time.** Opening a second source replaces or errors (TBD: `SourceAlreadyOpen` until pool lands).
- **No frame cache.** Repeated scrubs over the same range re-decode.
- **Consumer must drain events** or the worker blocks. No automatic backpressure relief in MVP.
- **No playback clock.** Real-time playback is the consumer’s responsibility (drive `NextFrame` on a timer).
- **No multi-threaded decode.** All decode work goes through one worker thread.
- **Single event consumer.** No built-in dispatcher to fan events out to multiple listeners.