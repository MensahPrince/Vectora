# Review: `cutlass-decoder` (Apple backend vs. main)

- **Date:** 2026-07-03
- **Branch:** `mobile-support` at `dbcd1de` (working tree included uncommitted changes)
- **Scope:** `src/apple.rs` compared against the `main` branch's decoder stack. This was an
  architecture comparison, not a line-by-line audit of every backend (`wmf.rs`, `android.rs`,
  `apple_audio.rs`, `probe.rs` were not reviewed in depth).

## Context

`apple.rs` has no counterpart on `main`. It is the Apple half of a wholesale replacement of the
FFmpeg-based decoder (`video/decoder.rs`, `video/hwaccel.rs`, keyframe indexer, audio analysis
modules) with platform-native decoders behind the new `cutlass_core::VideoDecoder` trait.

On `main`, decoding worked like this: `ffmpeg-next` demuxed (`avformat`) and decoded (`avcodec`),
optionally hardware-accelerated through FFmpeg's `AVHWDeviceContext` (VideoToolbox on macOS), and
every hardware frame was copied to CPU memory via `av_hwframe_transfer_data` before anything
downstream saw it. Seeking used a prebuilt keyframe index plus a `frame_at` roll-forward fast path
for sequential playback.

On this branch, `AVAssetReader` + `AVAssetReaderTrackOutput` do demux and VideoToolbox decode in
one step, vending IOSurface-backed `CVPixelBuffer`s directly.

## Gained vs. main

- **True zero-copy GPU path.** `OutputMode::Gpu` hands the renderer the live `CVPixelBuffer`
  pointer with a retained keep-alive (`RetainedImageBuffer`), so preview never copies pixels off
  the GPU. Main always transferred to CPU.
- **Full colorimetry.** Primaries / transfer / matrix parsed from the track's
  `CMFormatDescription`, including P3/BT.2020 and PQ/HLG — HDR is detectable. Main only tracked
  pixel format plus a full/limited-range flag.
- **Native bit depths.** 10-bit (`x420` → P010) and BGRA pass through as-is; main normalized
  everything to 8-bit YUV420p/NV12/RGBA.
- **Exact rational timestamps** (`CMTime` ↔ `RationalTime`) instead of stream ticks + `Duration`.
- **Rotation metadata** derived from `preferredTransform`.
- **Hermetic tests.** Integration tests synthesize a movie with `AVAssetWriter` and decode it
  back; main's tests silently skipped when local assets were missing.
- Dropping FFmpeg also removes a heavyweight dependency (binary size, LGPL/patent surface) —
  significant for mobile app-store distribution.

## Lost / not yet ported (risks)

1. ~~**Cheap seeking — the main performance regression risk.**~~ **Fixed 2026-07-03** (see
   below). `AVAssetReader` is forward-only, so `seek()` tears down and rebuilds the whole
   reader. Main had a keyframe index and a roll-forward path that avoided O(GOP²) re-decode
   during playback. Scrubbing is the primary touch interaction on mobile.
2. **Format breadth.** Only what AVFoundation opens on Apple platforms (Windows via Media
   Foundation, Android via MediaCodec; Linux TBD), versus FFmpeg's near-universal support.
   Inherent to the native-codec strategy; not addressable inside this crate.
3. **Audio analysis modules gone from this crate.** Beats / denoise / ducking / stretch are not
   ported; `apple_audio.rs` (`AvfAudioReader`) covers decode/resample only. Deliberate scope cut
   for the mobile pivot — those are engine-level features to re-port on demand, not decoder
   regressions.
4. **Android/Windows GPU parity.** Those backends currently ignore `OutputMode::Gpu` (no
   `AHardwareBuffer` / DXGI zero-copy import yet), so the zero-copy win is Apple-only for now.
   Android is likely the largest mobile audience. Still open: both need real target hardware to
   validate (this macOS host can only typecheck them), and both fail `OutputMode::Gpu` loudly
   rather than silently downgrading.

## Uncommitted delta observed at review time

On top of the staged new file: a `has_audio_track()` helper for the probe, track duration read
into `SourceInfo.duration` (was `None`), and the `CMTime` converters made `pub(crate)` for reuse
by `apple_audio.rs` / `probe.rs`.

## Fixes applied (2026-07-03)

- **Seek path (risk 1).** A shared roll-forward policy (`src/seek.rs`) now backs
  `VideoDecoder::frame_at` on **all three backends**: every backend records the PTS of the last
  frame it emitted, and a target strictly ahead of it by ≤ 1 s decodes forward instead of
  seeking (every in-between frame would be decoded after a seek anyway). Backward targets, long
  jumps, and fresh decoders still take the real seek, byte-identical to the default
  seek-then-walk. Measured on the hermetic H.264 fixture (300 sequential targets, release,
  Apple backend): **0.28 ms/frame rolled vs 5.99 ms/frame seek-per-frame, ~21×** (the ignored
  `bench_roll_forward_vs_seek_per_target` test reproduces this).
  - `resetForReadingTimeRanges` (the fix the old doc header floated) was evaluated and
    **rejected**: it only re-arms after the current range is fully drained, so a scrub seek
    would decode-and-drop the rest of the in-flight range — worse than the reader rebuild it
    saves. The rationale is captured in the `apple.rs` module docs.
  - Correctness is locked by unit tests on the policy (`seek.rs`) and AVFoundation integration
    tests (roll ≡ fresh-seek equivalence, backward/repeated targets, EOS recovery).
- **Apple: per-frame defensive copy removed.** `alwaysCopiesSampleData = NO` — neither output
  path mutates sample data (CPU copies planes itself, GPU retains the IOSurface), and Apple's
  header flags the default copy as a per-frame performance tax.
- **Windows parity with the Apple probe delta.** `SourceInfo.duration` is now read from
  `MF_PD_DURATION` (was hardcoded `None`, which zeroed `MediaProbe::frame_count`), and
  `wmf::has_audio_track` feeds `MediaProbe::has_audio` (was hardcoded `false` on Windows).
- **Apple reader lifecycle (found by benching on real assets).** Sustained scrub churn — every
  seek rebuilds the `AVAssetReader` — leaked each outgoing reader's in-flight decode pipeline
  (VideoToolbox session + queued IOSurfaces unwind asynchronously); after ~300 rebuilds against
  a 4K/60 source, process-wide decode resources were exhausted and **every** subsequent read
  (even on fresh decoders) failed with "Cannot Decode". Fixed by `cancelReading` on the outgoing
  reader in `seek()` and `Drop` (video + audio readers), plus an `autoreleasepool` around each
  frame/buffer pull (decode workers are plain Rust threads with no enclosing pool). Regression
  smoke test: `seek_churn_keeps_decoder_healthy`.
- Verified: `cargo test -p cutlass-decoder` on macOS (24 tests), `cargo clippy --all-targets`
  clean on host + `aarch64-linux-android` + `x86_64-pc-windows-gnu`.

## Measured on real assets (2026-07-03, M-series macOS, release)

`examples/decode_bench.rs` (`cargo run --release -p cutlass-decoder --example decode_bench --
<media> [seconds]`) against local Pexels footage:

| metric (CPU output) | 1080p30 H.264 | 4K60 H.264 |
| --- | --- | --- |
| probe | 1.2 ms | 2.5 ms |
| cold open → first frame | 28 ms (gpu: 13 ms) | 48 ms (gpu: 35 ms) |
| sequential `next_frame` | 0.45 ms/frame (~2 200 fps) | 1.65 ms/frame (~600 fps) |
| playback `frame_at` (roll-forward) | 0.45 ms/frame | 1.68 ms/frame |
| seek-per-frame (old path) | 34 ms/frame (29 fps) | 225 ms/frame (4.4 fps) |
| random scrub (uniform targets) | 31 ms/seek | 217 ms/seek |

Takeaways: `frame_at` adds no measurable overhead over raw sequential decode (ratio ≈ 1.0), the
roll-forward path is 75–135× faster than seek-per-frame on real media (which lands 4K60 playback
at ~600 fps vs 4.4 fps), and GPU output matches CPU decode throughput while halving cold-start.
Random scrub latency (~30 ms at 1080p, ~220 ms at 4K) is bounded by the source's keyframe
spacing — that cost is inherent to cold random access on long-GOP media; if it matters for UX it
needs an editor-level answer (proxies or a scrub cache), not a decoder change.

## Verdict

The architectural direction (native codecs, zero-copy surfaces, shared Rust control flow behind a
trait) is right for the mobile pivot. ~~The seek path and~~ Android zero-copy is now the follow-up
that matters before preview/scrub performance is evaluated on device; the seek path is fixed and
measured above.
