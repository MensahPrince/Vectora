# Decoder roadmap

Implementation plan for the **`decoder`** crate. Follows the design in **`decoder-research.md`**. Build in this order — each phase ends in something **runnable and tested** before moving on. Don’t skip ahead; the later phases assume the earlier ones compile and pass.

Scope reminder: **library-only**, **CPU decode**, **single-threaded API**, **video only**. No engine, no pool, no async, no hwaccel, no audio.

---

## Phase 0 — Project setup & toolchain smoke test

**Goal:** prove `ffmpeg-next` links and runs on your machine **before** writing any of your own code.

**Tasks:**

1. Create workspace crate `crates/decoder` (lib).
2. Install system FFmpeg (`brew install ffmpeg pkg-config` on macOS — you’re on macOS, ffmpeg-next needs pkg-config to find the dylibs).
3. Add deps to `crates/decoder/Cargo.toml`:
   - `ffmpeg-next` (latest stable)
   - `thiserror` (for the error enum later)
4. Write `examples/hello_ffmpeg.rs`:
   ```rust
   fn main() {
       ffmpeg_next::init().unwrap();
       println!("ffmpeg version: {}", ffmpeg_next::util::version());
   }
   ```

**Deliverable:** `cargo run --example hello_ffmpeg` prints the version. If linking fails here, fix it now — it only gets harder once real code depends on it.

**Common gotchas on macOS:**

- `pkg-config not found` → `brew install pkg-config`.
- Apple Silicon vs Intel brew prefix differences — `PKG_CONFIG_PATH` may need to point at `/opt/homebrew/lib/pkgconfig`.
- If you’re on Bun-driven scripts elsewhere, this is pure Cargo — no Bun involved.

---

## Phase 1 — Core types (no decoding yet)

**Goal:** all public types from the design doc exist, compile, and have unit tests. Zero FFmpeg calls beyond linkage.

**Modules:**

- `time` — `Rational { num: i64, den: u32 }`, conversion helpers (to/from `f64` for display only, never as truth).
- `pixel` — `PixelFormat` enum (`#[non_exhaustive]`, variants `Yuv420p`, `Nv12`, `Rgba8`).
- `frame` — `Plane { data: Vec<u8>, stride: usize }`, `CpuFrame { format, planes }`, `FrameData::Cpu(CpuFrame)` (single variant for now, but **enum** so Gpu can be added later without breaking), `DecodedVideoFrame { width, height, pts, timebase, data }`.
- `outcome` — `DecodeOutcome::{ Frame(DecodedVideoFrame), Eof }`.
- `error` — `DecoderError` with `thiserror::Error`, variants `Open`, `Seek`, `Decode`, `Unsupported { what: String }`, `Io`. Wrap `ffmpeg_next::Error` inside the relevant variants via `#[from]` or explicit conversion.
- `source` — `SourceInfo { width, height, timebase: Rational, duration: Option<Rational>, pixel_format: PixelFormat }`.

**Tests:**

- Rational equality, simplification, conversion round-trips.
- `PixelFormat` exhaustive `match` exercise (catch unintended additions).
- `DecoderError` `Display` impls produce useful strings.

**Deliverable:** `cargo test -p decoder` passes. No `Decoder` struct yet — just the vocabulary.

---

## Phase 2 — `Decoder::open` + probe

**Goal:** open a file, pick the best video stream, expose `SourceInfo`. **No frame decoding yet.**

**Tasks:**

1. Add `OnceLock` for `ffmpeg::init()` — call lazily on first `open`.
2. `pub struct Decoder` holds:
   - `ffmpeg_next::format::context::Input` (demuxer)
   - `ffmpeg_next::codec::decoder::Video` (codec context)
   - `stream_index: usize`
   - `timebase: Rational`
   - `info: SourceInfo`
3. `Decoder::open(path: &Path) -> Result<Self, DecoderError>`:
   - Open input via `ffmpeg::format::input(&path)`.
   - Find best video stream: `input.streams().best(Type::Video)`.
   - Build decoder from the stream’s codec parameters.
   - Read pixel format, dimensions, timebase → `SourceInfo`.
   - Map ffmpeg pixel format to your `PixelFormat`. If unsupported → `DecoderError::Unsupported`.
4. `Decoder::info(&self) -> &SourceInfo`.

**Test asset prep** (do this once, commit to `crates/decoder/tests/assets/`):

```bash
# 5-second test pattern, 30fps, GOP=30, H.264. Tiny file, perfect for tests.
ffmpeg -f lavfi -i testsrc=duration=5:size=320x240:rate=30 \
  -c:v libx264 -g 30 -pix_fmt yuv420p tests/assets/testsrc_h264.mp4

# Same but with B-frames forced — for later seek tests.
ffmpeg -f lavfi -i testsrc=duration=5:size=320x240:rate=30 \
  -c:v libx264 -g 30 -bf 3 -pix_fmt yuv420p tests/assets/testsrc_bframes.mp4
```

**Tests:**

- Open `testsrc_h264.mp4`, assert width=320, height=240, pixel_format=Yuv420p, timebase has expected den.
- Open a non-existent path → `DecoderError::Open`.
- Open a non-video file (e.g. a `.txt`) → `DecoderError::Open` or `Unsupported`.

**Deliverable:** integration test opens a real file and probes it. Decoder builds without panicking.

---

## Phase 3 — Forward decode (`next_frame`)

**Goal:** pump packets through the decoder and return owned `DecodedVideoFrame`s in presentation order. **This is where stride and plane bugs live — pay attention.**

**Tasks:**

1. Implement the **send-packet / receive-frame** loop:
   - Read packets from the demuxer matching `stream_index`.
   - `send_packet` to decoder.
   - Drain `receive_frame` calls until decoder asks for more input.
   - On end of input, send a flush packet (`send_eof`) and drain final frames.
2. Convert `ffmpeg_next::frame::Video` → `DecodedVideoFrame`:
   - For each plane the format defines, copy `frame.data(i)` slice into an owned `Vec<u8>`, sized **`frame.stride(i) * plane_height(format, height, i)`**.
   - Record `frame.stride(i)` per plane — **do not** assume `stride == width × bpp`.
   - PTS: `frame.pts()` is in stream timebase units → store as `Rational { num: pts, den: timebase.den }` (or however your `Rational` is laid out).
3. Pixel format handling for v1:
   - **YUV420P**: 3 planes. Plane 0 (Y) full size, planes 1-2 (U, V) at `height / 2`, each row `stride[i]` bytes.
   - **NV12**: 2 planes. Plane 0 (Y) full size, plane 1 (interleaved UV) at `height / 2`.
   - Anything else → `DecoderError::Unsupported`.

**The plane height rule that bites people:**

```
plane 0 (Y or RGB):    height
plane 1 (U / UV):      height / 2   (for 4:2:0)
plane 2 (V):           height / 2   (for 4:2:0)
```

Get this wrong and your test will either crash on out-of-bounds or produce green/purple frames downstream.

**Tests:**

- Decode all frames from `testsrc_h264.mp4`. Expect 150 frames (5s × 30fps). Assert: first PTS is 0, PTSes are monotonically increasing.
- After exhausting frames, next call returns `Ok(DecodeOutcome::Eof)`.
- Frame 0 has `width = 320`, `height = 240`, 3 planes for YUV420P, plane 0 data length == `stride[0] * 240`.

**Deliverable:** can decode an entire file end-to-end. Frame data is owned, sized correctly, stride preserved.

---

## Phase 4 — `seek_exact`

**Goal:** seek to an arbitrary timestamp and return the frame whose presentation PTS matches it (or the closest decoded frame ≥ target — define this in the doc comment).

**Tasks:**

1. `seek_exact(&mut self, target: Rational) -> Result<DecodeOutcome, DecoderError>`:
   - Convert target to stream timebase units.
   - `input.seek(target_ts, ..target_ts)` with `AVSEEK_FLAG_BACKWARD` (the `ffmpeg-next` API exposes this via `seek` on the format context — check the version you’re on).
   - `decoder.flush()` — discards stale reference state.
   - Run the same packet/frame pump as `next_frame`, but **discard** every frame whose PTS < target.
   - Return the first frame whose **presentation PTS** ≥ target.
2. **B-frame correctness:**
   - PTS, not DTS, is what you compare against the target.
   - Frames may arrive out of decode order but with correct PTS — trust PTS for the discard decision.
3. **Past-EOF behavior:**
   - If you drain to EOF without finding a matching frame → return `Ok(DecodeOutcome::Eof)`. Engine clamps.

**Tests:**

- Seek to `Rational { num: 60, den: 30 }` (= 2.0s) on `testsrc_h264.mp4`. Expect returned frame with PTS exactly `60/30` (or equivalent reduced form).
- Repeat on `testsrc_bframes.mp4` — same expected behavior despite reorder.
- Seek to a target past the end → `Ok(Eof)`.
- Seek to 0 → first frame.

**Deliverable:** frame-accurate seek works on both B-frame and no-B-frame files. This is the hard one — budget more time here than the others.

---

## Phase 5 — `seek_scrub`

**Goal:** fast keyframe-snap seek. Should be trivial after Phase 4.

**Tasks:**

1. `seek_scrub(&mut self, target: Rational) -> Result<DecodeOutcome, DecoderError>`:
   - Same backward seek + flush as `seek_exact`.
   - **Return the first decoded frame**, regardless of its PTS. No discard loop.
2. Document on the method: “Returned frame’s PTS is ≤ target, snapped to the last keyframe at or before target. Cost is O(GOP), independent of distance from current position.”

**Tests:**

- Scrub to 2.0s on `testsrc_h264.mp4` (GOP=30, so keyframes at PTS 0, 30, 60, 90, 120). Expect returned frame’s PTS == 60/30 = 2.0s (in this case it lands exact because target is a keyframe).
- Scrub to 2.5s (PTS = 75/30) → expect returned frame’s PTS == 60/30 (snap to prior keyframe).
- Scrub past EOF → `Ok(Eof)` or last keyframe — pick one, document it.

**Deliverable:** scrub returns a frame in bounded time regardless of seek distance.

---

## Phase 6 — Error hardening, docs, README

**Goal:** ship-quality library hygiene.

**Tasks:**

1. Audit every `?` and `unwrap` in the crate. Replace ad-hoc errors with the right `DecoderError` variant. Preserve underlying `ffmpeg::Error` where it adds debugging value.
2. Doc comments on every public item:
   - `Decoder` — lifetime, threading note (`!Sync`, designed for engine-owned worker).
   - `seek_scrub` vs `seek_exact` — precise semantics, what PTS is guaranteed.
   - `DecodeOutcome::Eof` — when it’s returned, what it means after seek vs after pump.
3. `README.md` in the crate: 1-screen overview, quickstart, link to `decoder-research.md`.
4. Run `cargo clippy -p decoder --all-targets -- -D warnings`. Fix everything.
5. Run `cargo doc -p decoder --no-deps --open`. Confirm rendered docs are coherent.

**Deliverable:** `cargo doc`, `cargo clippy`, `cargo test` all clean. Crate is documented well enough that engine devs can integrate without reading source.

---

## Phase 7 — Example binary (`dump-frames`)

**Goal:** a small CLI that exercises the public API end-to-end. Useful as future debugging tool and as living documentation.

**Tasks:**

1. `examples/dump_frames.rs`:
   - Args: `<input-path> [--seek <seconds>] [--exact|--scrub] [--count <n>]`.
   - Open file, print `SourceInfo`.
   - If `--seek` provided, do `seek_exact` or `seek_scrub` accordingly.
   - Pump `--count` (default 10) frames, print PTS and plane sizes for each.

**Deliverable:** `cargo run --example dump_frames -- tests/assets/testsrc_h264.mp4 --seek 2.0 --exact --count 5` runs and prints reasonable output.

---

## Test assets reference

All in `crates/decoder/tests/assets/` (commit them — they’re small):

| File | Purpose |
|---|---|
| `testsrc_h264.mp4` | 5s, 30fps, GOP=30, H.264 YUV420P. |
| `testsrc_bframes.mp4` | Same, with B-frames (`-bf 3`). |
| `test_av.mp4` | H.264 + AAC; verifies non-video packets are skipped. |
| `audio_only.m4a` | AAC only; `open` → no video stream. |
| `test_unsupported_codec.mkv` | FFV1 / non-v1 pixel format; `Unsupported` at open. |
| `corrupt_truncated.mp4` | Invalid bytes; demuxer `Open` error. |

All are small. Recreate with `crates/decoder/tests/assets/regenerate.sh` (requires `ffmpeg`).

---

## Order-of-operations rule

**Don’t parallelize phases.** Tempting to start `seek_exact` while `next_frame` is half-working — don’t. Seek correctness depends on the pump being right. A green test suite at the end of each phase is the only reliable signal you haven’t broken anything subtle (especially around PTS rounding and B-frame reorder).

## Out of scope (still — same as the design doc)

Hardware decode, audio, async API, `FramePool`, decoder pooling, color management beyond rec.709 assumption, proxy generation, index building. Resist scope creep — engine integration comes next, not more decoder features.