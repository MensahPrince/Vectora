# Decoder research

## Long-GOP and why seeking is two different problems

Long-GOP codecs (H.264, HEVC, AV1, etc.) store most frames as **predicted** pictures that depend on **reference** frames. Only **intra** (I) frames / IDR sync points are safe places to **restart decoding** without already having decoder state.

**Demuxer seek** operates on timestamps and byte offsets, not “pixel frame *N*.” In practice, **accurate display at presentation time *t*** is almost always implemented as:

1. **Seek backward** to the **last sync point** (keyframe / entry point) at or before *t*.
2. **Flush** the video decoder so stale references are discarded.
3. **Decode forward**, discarding output until the frame whose **presentation timestamp (PTS)** matches the target (with care for **decode order vs presentation order** when **B-frames** are present).

Seeking to a **non-keyframe packet** without decoding forward (e.g. some “seek to any packet” modes) can yield **invalid or gray** images until the next clean sync — generally unacceptable for an editor preview.

**Two clocks:** **PTS** is what the user cares about for “where on the timeline am I?” **DTS** (and codec reordering) is what the **bitstream order** cares about. Robust seek logic must respect both.

References:

- [Frame-accurate seeking in Long-GOP video (VID, 2025)](https://vid.co/blog/frame-accurate-seeking-in-long-gop-video) — editorial summary of I/P/B, index quality, “provisional” preview vs locked frame, integer timebases.
- FFmpeg / libav discussions and Stack Overflow on **`av_seek_frame`**, **`AVSEEK_FLAG_BACKWARD`**, **`avcodec_flush_buffers`**, and avoiding **`AVSEEK_FLAG_ANY`** for correct pictures on inter-coded video.

## Demuxer seek: stream PTS vs `AV_TIME_BASE` (`ffmpeg-next` caveat)

**Problem:** In `ffmpeg-next`, [`format::context::Input::seek`](https://docs.rs/ffmpeg-next) wraps `avformat_seek_file` with **stream index `-1`**. In FFmpeg, that mode interprets the seek timestamp in **`AV_TIME_BASE`** units (internal time scale), **not** in the **video stream’s** `time_base` (PTS ticks).

**Symptom:** Passing “media seconds × stream timebase” into `Input::seek` looks correct on paper but seeks the wrong place (often near the start). Scrub/exact both break in subtle ways.

**Cutlass fix:** The `decoder` crate calls **`avformat_seek_file` directly** with the **selected video `stream_index`**, timestamps in **stream PTS units**, and **`AVSEEK_FLAG_BACKWARD`**, then `avcodec_flush_buffers` on the decoder. If you fork or wrap seek, keep this contract — do not round-trip arbitrary PTS through `Input::seek` without verifying stream index and units.

## Scrubbing vs committed seek (industry pattern)

**Scrubbing** means the user (or UI) requests **many targets in quick succession** while dragging the playhead. Doing a full **sync-point seek + decode-forward-to-exact-PTS** on **every** micro-move is often **CPU-heavy** and can feel **laggy** or **flickery** when GOPs are long — small timeline motions can bounce between adjacent sync points.

Common pattern:

- **While scrubbing:** show something **fast** and **approximate** — e.g. **nearest keyframe** image, **low-res proxy**, or **cached I-frame thumbnail** — without guaranteeing **PTS == requested *t***.
- **On release / play / edit action:** perform **accurate** seek so the **displayed frame matches the requested time** (or document frame index), suitable for trimming, export previews, and QC.

**mpv** is a useful reference: the on-screen seek bar’s drag path favors **faster keyframe-aligned** behavior, while **high-resolution / exact** seeking is a separate policy; users report **flicker between keyframes** when dragging slowly if the implementation sticks to sync-point jumps ([mpv#4183](https://github.com/mpv-player/mpv/issues/4183), [manual — hr-seek and defaults](https://mpv.io/manual/master/)).

**Shotcut / MLT** adjustments (e.g. buffer and real-time mode around **pause vs playback**) show that **frame accuracy** is often a **pipeline policy** problem as much as a decoder detail: same underlying seek math, different **when** you demand exact frames.

**DaVinci Resolve–style** workflows: **proxy / optimized media** reduces decode cost during interactive editing; final output still uses **originals**. That is **orthogonal** to scrub-vs-exact policy but changes **how expensive** each mode feels.

## Implications for Cutlass

- The **decoder** crate should own **demux + decode**, not timeline/UI. It should speak in **time + frames + buffer metadata**.
- **Two explicit seek/display policies** — don’t implement a **slow middle ground** that is neither buttery scrub nor frame-accurate stills:
  - **Scrub (keyframe snap):** **seek backward** to the sync point at or before the target, **flush**, then **decode and return the first displayed picture** — i.e. **one** output frame after the seek, **no decode-forward** toward the requested PTS. Preview may **jump between keyframes** as the playhead moves (mpv-style); the UI stays responsive.
  - **Exact:** **same backward seek + flush**, then **decode forward** until the frame whose **presentation PTS** matches the requested time (per stream/timebase rules), suitable for release, trim, and export preview. Engine maps domain-specific cases (e.g. seek-past-EOF) using outcomes/errors defined below.

## Shared types crate (when and how)

Introduce a small **`cutlass-core`** (or **`cutlass-media`**) crate **when a second workspace crate needs the same type** — e.g. rational timeline time, `DecodedVideoFrame` metadata, media/source IDs, shared errors. That keeps **decoder** from depending on **renderer** and avoids copy-paste.

- **Start narrow:** one or two modules (`time`, `media`) rather than a giant `types.rs`.
- **Avoid a junk drawer:** if only **decoder** needs a type **today**, keep it in **decoder** and **promote** it when reuse appears.
- **Slint-first types** (per project rules) stay defined in **`.slint`** where the UI must own them. The shared Rust crate is for **engine/media** semantics; bridge explicitly at **app/engine** boundaries instead of duplicating Slint structs in Rust-only core.

## API shape: `struct` + `impl` (session object)

The primary API is a **`Decoder` struct** holding **per-source state**: demuxer + codec context, chosen stream index, time base, etc. Methods like `open`, `seek_scrub`, `seek_exact`, and frame output live on **`impl Decoder`**.

- Matches **one open media source** per instance — “OOP style” as **state + behavior**, which is idiomatic Rust (composition over inheritance).
- **`ffmpeg::init()`:** call **inside the `decoder` crate** behind **`std::sync::OnceLock`** (or `Once`) the first time `Decoder::open` runs. **Consumers must not** remember to init FFmpeg at the app/engine callsite; the crate stays self-contained.
- **Pure helpers** (timebase math, PTS conversion) can stay as **private methods** or small **module functions** without a `self`.

**Sketch (names TBD):**

```rust
impl Decoder {
    pub fn open(path: &Path) -> Result<Self, DecoderError>;
    pub fn info(&self) -> &SourceInfo; // dimensions, timebase, duration, pixel format

    /// Seek + decode + return ONE frame (keyframe snap). No forward scan.
    pub fn seek_scrub(&mut self, target: Rational) -> Result<DecodeOutcome, DecoderError>;

    /// Seek + decode forward until presentation PTS matches target.
    pub fn seek_exact(&mut self, target: Rational) -> Result<DecodeOutcome, DecoderError>;

    /// Pump the next frame in presentation order (for playback).
    pub fn next_frame(&mut self) -> Result<DecodeOutcome, DecoderError>;
}
```

## Threading: `Send` / `Sync` and who owns the worker

**This is a design decision, not a footnote.** With **`ffmpeg-next`**, codec/demuxer state is **thread-confined** in practice: treat **`Decoder` as `!Sync`** and **not safe to share** across threads for concurrent `send_packet` / `receive_frame`. Don’t pretend “we’ll document it later.”

**Chosen direction: passive `Decoder` + engine-owned worker.**

- **`Decoder`** is a **synchronous, in-process object**: the engine (or pool) **holds it on one dedicated decode thread** and **pumps** it by calling methods. **No** hidden `spawn` inside the `decoder` crate; **no** self-owning thread inside `Decoder` exposing an **async**/`Future` API **in v1**.
- The **engine** talks to that thread via **channels** (commands in, frames/errors out). That keeps pooling, scheduling, cancellation, and backpressure **explicit** and **testable** without surprise tasks.
- **Alternative rejected for now:** a `Decoder` that **spawns its own** worker and exposes async-only API — harder to test in isolation, splits scheduling between engine and library, and obscures lifecycle.

The **decoder pool** also lives **behind** that same worker boundary: one thread owns `HashMap<MediaSourceId, Decoder>` (or equivalent), not `Arc<Mutex<Decoder>>` contended from UI threads.

## Frame data: ownership, layout, and forward-compat

### Owned payloads in v1

**The public API returns owned pixel data.** The renderer may hold a frame for a few milliseconds while uploading to **wgpu**; the decoder must be free to produce the next frame in parallel without a “release this borrow before the next call” contract.

- **Borrowed** views tied to internal FFmpeg buffers force **sequential** decode-then-upload and **kill overlap** between decode and GPU work.
- A dedicated **`FramePool`** (reuse allocations) is a **later** optimization if profiling demands it — **after** the owned API is stable.

### Layout: planes + strides + explicit pixel format

“Owned bytes, layout TBD” is a trap. wgpu uploads care about **bytes-per-row** (which is **not** `width × bpp` because FFmpeg aligns rows to 16 / 32 / 64 byte boundaries). YUV planar formats need **per-plane** data + strides. Lock the shape down now.

**Sketch (names TBD):**

```rust
pub struct DecodedVideoFrame {
    pub width: u32,
    pub height: u32,
    pub pts: Rational,        // presentation time
    pub timebase: Rational,   // stream timebase
    pub data: FrameData,
    // Color metadata (range, primaries, transfer, matrix) deferred — assume rec.709 limited for v1.
}

pub enum FrameData {
    Cpu(CpuFrame),
    // Future: Gpu(GpuFrame) — VideoToolbox / NVDEC / DXVA surfaces.
    // Shape reserved now so adding hwaccel later is NOT a breaking change.
}

pub struct CpuFrame {
    pub format: PixelFormat,
    pub planes: Vec<Plane>,   // YUV420P = 3, NV12 = 2, RGBA = 1
}

pub struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,        // bytes per row (>= width * bytes_per_sample)
}

#[non_exhaustive]
pub enum PixelFormat {
    Yuv420p,
    Nv12,
    Rgba8,
    // Add as needed; non_exhaustive keeps callers honest.
}
```

**v1 supported formats:** **YUV420P** and **NV12** (covers the overwhelming majority of H.264/HEVC sources). Anything else → `DecoderError::Unsupported { format }`. Convert at the **renderer** (a shader does YUV→RGB cheaper than a CPU swscale pass).

### Hardware decode is reserved, not implemented

`FrameData::Cpu` is the only variant today. The **enum exists** so adding `FrameData::Gpu(...)` for VideoToolbox (macOS), NVDEC, or DXVA later is **additive**, not a major version bump. Consumers must `match` exhaustively today, which forces them to handle the future variant gracefully.

## Rational time in metadata

**Use rational time (numerator / denominator) in frame metadata from day one**, not `f64` as the source of truth for PTS / duration / stream timebase. Floats drift on long timelines; **num/den** is what serious pipelines use. Convert to float only for **display** if needed.

## Error model (library crate)

**Do not use `anyhow` (or equivalent type-erased errors) as the primary surface** of the **`decoder`** library — callers need to react to specific failures (**seek past EOF** → clamp to last frame, **unsupported codec** → user message, etc.).

### EOF is not an error

End-of-stream is a **normal outcome**, not a failure. Mixing it into `DecoderError` forces the engine to `match` on an “error” type during the happy playback path. Separate it cleanly:

```rust
pub enum DecodeOutcome {
    Frame(DecodedVideoFrame),
    Eof,
}
```

Both `seek_*` and `next_frame` return `Result<DecodeOutcome, DecoderError>`. Seek past end of media yields `Ok(DecodeOutcome::Eof)`; the **engine** decides whether to clamp to last frame, show black, etc.

### `DecoderError` variants (names TBD)

- **`Open { source }`** — probe / open / stream selection failed
- **`Seek { source }`** — demuxer / seek failed
- **`Decode { source }`** — codec decode failed
- **`Unsupported { what }`** — codec, pixel format, or container limitation
- **`Io { source }`** — file / stream read error

**Preserve** the underlying **`ffmpeg::Error`** (or a thin wrapper) inside variants where relevant so logs and bug reports stay actionable. Engine/UI can map these to user-facing strings without losing the discriminant.

**`cutlass-core`:** when we add a shared crate, **`DecoderError`** can move there **if** multiple crates need the same enum — start in **`decoder`** until a second consumer exists (same rule as other shared types).

## Scrub cancellation contract

Scrubbing is **upstream-coalesced**, not in-decoder-cancellable. The engine must drop stale scrub commands before they reach the decoder rather than asking the decoder to abandon mid-flight work.

**Why this works:**

- **`seek_scrub` is designed cheap** — one backward seek + flush + **one** decoded frame. No forward scan. Cost is bounded by GOP length, not playhead distance.
- The engine’s command channel to the decode worker should use **latest-wins** semantics for scrub requests: when a new scrub target arrives, **drop any pending earlier scrub** in the channel (single-slot replace, or drain + take-last).
- The decoder finishes whatever scrub it’s currently mid-flight on (cheap), then immediately picks up the freshest target.

**What the decoder crate guarantees:** scrub seeks are O(GOP), not O(distance from current PTS). What the engine guarantees: it never floods the worker with stale targets faster than they can be drained.

`seek_exact` is **not** designed for cancellation. It runs to completion — used on mouse-up / commit, not during drag.

## Decoder pool and multiple clips / sources

A timeline has **many clips**; clips reference **different files** (or the same file at different offsets — still one demuxer **per open path** unless we model sub-clips differently later). Each **`Decoder`** is **heavy** (open FDs, codec state, possible hw contexts). You usually **do not** want **unbounded** simultaneous opens.

**Model:**

- **One `Decoder` instance ≈ one “open” media source** (typically one path / one logical asset stream after probe).
- **`engine` (or a dedicated cache layer)** owns a **pool**: e.g. `HashMap<MediaSourceId, Decoder>` on the **decode worker thread** with **LRU eviction** and a **max concurrent decoders** cap — **not** a contended `Mutex` around `Decoder` on the UI thread.
- **Same path, two clips:** reuse one pooled entry; seek when switching which clip is “active” for preview (or keep **hot** sources pinned while visible).

**Split of responsibility:**

- **`decoder` crate:** `Decoder` lifecycle (`open`, `drop`), seek semantics, frame output — **no** knowledge of timeline clips.
- **Engine:** maps **clip → `(source_id, time_in_media)`**, sends commands to the **decode worker**, owns pool eviction and **throttling / coalescing** during scrub.

**Future:** the pool may hold **soft references** to **proxy** paths vs **originals** (engine chooses URL); decoder stays **path-agnostic**.

## Known limits (v1)

These are deliberate v1 constraints. Documented so future-us knows they were chosen, not missed.

- **Single decode worker thread caps total decode throughput.** Fine for one preview window. **Not** sufficient for: multi-track simultaneous preview (A-roll + B-roll PiP), background thumbnail extraction running alongside playback, multi-stream export. Future shape is **one worker per `Decoder`** or a small **worker pool** (N threads, M decoders, scheduler matches work to threads). Doesn’t change the `Decoder` API — only how the engine pumps them.
- **No hardware decode.** All decode is software / CPU. `FrameData::Gpu` variant is reserved so adding hwaccel later is additive.
- **No async API in `decoder` crate.** Engine-owned worker + channels is the integration pattern; if we later want a `tokio`-friendly facade, it lives in **engine**, not **decoder**.
- **No proxy generation, no audio, no index building.** Decoder only.
- **Color metadata defaults to rec.709 limited.** Real color management deferred until renderer needs it.

## What we are doing today (decoder scope)

Today’s work stays **small and library-only**:

1. **Dependencies:** bring **`ffmpeg-next`** into the **`decoder`** crate; **`ffmpeg::init()`** via **`OnceLock`** on first `Decoder::open`.
2. **API shape (initial):** `Decoder::open`, best-video-stream selection, **`seek_scrub`** (keyframe snap, **one** decoded frame, no forward scan), **`seek_exact`** (decode forward to target PTS), **`next_frame`** (playback pump), plus **typed errors** and **`DecodeOutcome`** (see above).
3. **Output:** **owned** `DecodedVideoFrame` with **per-plane data + strides**, **explicit `PixelFormat` enum** (YUV420P, NV12 in v1), **rational PTS + timebase**, wrapped in `FrameData::Cpu` so hwaccel is forward-compatible — **no** WGPU, **no** Slint, **no** timeline.
4. **Threading:** **passive** `Decoder` documented as `!Sync`, designed to live on an **engine-owned worker thread** + channels. Library tests may call `Decoder` directly on the test thread.

**Out of scope for today:** hardware decode, zero-copy GPU surfaces, audio, index building, proxy generation, **`FramePool`**, an async API inside **`decoder`**, color management beyond rec.709 assumption, and **decoder pooling** (engine concern once `Decoder` is solid).