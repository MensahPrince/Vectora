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

---

# Review: Apple decoder vs. open-GOP H.264 (`export.mp4` "Cannot Decode")

- **Date:** 2026-07-10
- **Branch:** `macos-dev` at `73c49ad` (clean tree)
- **Scope:** critical review of `src/apple.rs` (+ `src/seek.rs` interplay), driven by a real
  failing asset: `~/Movies/Cutlass/assets/export.mp4` (4K H.264 High, 23.976 fps, 104 s,
  48 Mb/s, video-only, muxed by Lavf62).
- **Repro:** `cargo run --release -p cutlass-decoder --example decode_bench -- <file> 3`
  (random-scrub phase panics with `Cannot Decode`); `examples/seek_probe.rs` (added with this
  review) isolates the mechanism.

## Root cause

The asset is an **open-GOP** encode. Its container flags 12 sync samples (~10.4 s GOPs), but
only frame 0 is a true IDR — every other "keyframe" is a **non-IDR I-frame**, and most are
followed in decode order by **leading B-frames** that display before the I-frame and reference
the *previous* GOP (NAL inspection: `SEI, non-IDR` at every sync sample except t=0).

`AvfDecoder::seek` rebuilds the `AVAssetReader` with `timeRange.start = target`. The reader
starts decode at the container sync sample at/before the target. When that sync sample has
leading B-frames, VideoToolbox hits references that don't exist and — instead of dropping those
frames the way FFmpeg does — **fails the whole reader session**: the first
`copyNextSampleBuffer` returns NULL with status `Failed` and `AVErrorDecodeFailed`
("Cannot Decode"). Zero frames are delivered.

Measured correlation is exact (cold open → `seek(t)` → `next_frame()`):

| seek target | effective sync sample | leading B-frames | result |
| --- | --- | --- | --- |
| 5 s | 0.000 (IDR) | 0 | OK, pts 5.005, 401 ms |
| 15 s | 10.427 (non-IDR) | 0 | OK, pts 15.015, 292 ms |
| 30 s | 20.854 (non-IDR) | 1 | **Cannot Decode** |
| 60 s | 53.387 (non-IDR) | 1 | **Cannot Decode** |
| 90 s | 83.292 (non-IDR) | 0 | OK, pts 90.007, 517 ms |
| 100 s | 93.719 (non-IDR) | 1 | **Cannot Decode** |

8 of the 11 non-IDR sync samples have leading B-frames, so **most of the clip's timeline is a
dead zone for random access** while remaining perfectly decodable sequentially:

- `ffmpeg -xerror` decodes all 2 490 frames with zero errors (the file is valid; reference
  decoders drop undecodable leading frames on open-GOP seeks instead of failing).
- The AVF backend itself decodes across the bad boundaries fine when it *arrives* sequentially
  (seek 19 s → roll past 20.854 → OK): the references exist by then.
- Pulling the reader start back past the bad sync sample recovers: target 60 s fails from
  starts 59/58/56 s (same effective sync, 53.387) and succeeds from 52 s (effective sync
  42.960, leading-B = 0), total 1.19 s including the failed attempts.

## Findings (ranked)

1. **Critical — one undecodable leading frame kills the whole read, with no retry**
   (`apple.rs::next_pixel_frame` / `seek`). Coupling demux+decode inside `AVAssetReader` means
   the backend inherits VideoToolbox's strictness with no way to skip a bad frame. Any consumer
   that seeks — scrub, filmstrip thumbnails, trimmed clip in-points, transitions, export of a
   trimmed timeline — errors out on 80 % of this file; `render.rs::decode()` propagates the
   `Err`, so a single failed seek aborts the whole composite/export. Fix directions, cheapest
   first:
   - *Retry with back-off (small, contained):* when the reader fails having delivered **zero
     frames since `startReading`**, rebuild with `timeRange.start` pulled back (double the
     back-off: 1 s, 2 s, 4 s … clamped to 0, where sample 1 is stss-guaranteed and frame 0 is
     IDR here) and walk forward to the original target — `frame_at`'s walk already does this.
     Verified above at ~1.2 s worst case for this asset; correct because sequential decode
     through the same boundaries succeeds.
   - *Exact back-off:* `AVSampleCursor` (macOS: `AVAssetTrack.canProvideSampleCursors`) can
     step sync samples directly instead of guessing seconds.
   - *Structural (bigger):* demux with nil `outputSettings` (compressed `CMSampleBuffer`s) and
     own the `VTDecompressionSession`, dropping frames that fail — FFmpeg semantics, per-frame
     error tolerance, and it would also unlock decoder reuse across seeks.

2. **High — a failed reader is sticky on the roll path.** After a read failure `started` stays
   true and `last_pts` keeps the pre-failure PTS, so any subsequent target inside the roll
   window rolls onto the dead reader (`status == Failed`) and errors again; only a backward/far
   target happens to rebuild via `seek`. During playback (sequential targets, always in-window)
   the decoder never self-heals. On read failure the backend should tear down the reader (and
   clear `last_pts`) so the next call rebuilds.

3. **Medium — error diagnostics carry nothing.** `reader_error` surfaces only
   `localizedDescription` ("Cannot Decode"). Include the `NSError` domain + code
   (`AVFoundationErrorDomain` / `AVErrorDecodeFailed`), the underlying `NSUnderlyingError`
   (VT OSStatus), and the reader's `timeRange.start` / last-emitted PTS. This investigation had
   to reconstruct all of that from scratch; a field report with just "Cannot Decode" is
   undebuggable.

4. **Medium — `frame_at_nearest` has no cost advantage on this backend.** `AVAssetReader`
   trims output to `timeRange`, so the "seek + one decode" snap path still decodes the whole
   GOP prefix internally and returns the frame at/after the target — on long-GOP 4K that is
   hundreds of ms to seconds per thumbnail, and on this asset it fails identically to the
   exact path. The `seek.rs` doc's "single decode replacing the GOP-prefix walk" holds for
   MF/MediaCodec, not here. Worth documenting at least; the structural fix in (1) would also
   make true sync-frame snapping possible.

5. **Low — `frame_rate_of` prefers `minFrameDuration`,** which overestimates fps on VFR
   sources → `MediaProbe::frame_count` over-reports → EOF-overshoot churn (mitigated by
   `seek_back_to_tail`, but each miss costs `MAX_EOF_BACKSTEPS` seek attempts worst case).
   Fine on this asset (constant 23.976).

6. **Low — misc.** `probe`/`has_audio_track`/`audio_track_duration` each build their own
   `AVURLAsset` (moov re-parse per call); `rational_to_cmtime` can overflow `value·den` for
   extreme time bases; `coded_size` is set from `naturalSize`, which is display size (benign —
   frames carry real buffer dimensions).

## What holds up

The reader-lifecycle work (`cancelReading` on replace/drop, per-pull `autoreleasepool`) is
correct and guards a real resource-exhaustion failure; the roll-forward window design keeps
this 4K asset at ~2.7 ms/frame sequential vs ~136 ms/frame seek-per-frame; the zero-copy GPU
path's retain semantics are sound; colorimetry parsing is thorough. The failure here is not
the architecture being wrong — it is the missing failure-recovery story around
`AVAssetReader`'s all-or-nothing decode sessions.

## Verdict

`apple.rs` is correct for closed-GOP media and fast for the paths it optimized, but it treats
`AVAssetReader` as infallible on valid files, and open-GOP H.264 (any x264 `--open-gop` /
some hardware-encoder output) breaks that assumption on **every seek into 8 of this file's 11
GOPs**. Finding 1's back-off retry plus finding 2's teardown-on-failure are small, testable
changes that would take this asset from "export fails" to "works with a one-second worst-case
seek penalty"; the demux/decode split is the durable fix if open-GOP sources matter broadly.

## Fixes applied (2026-07-10)

All six findings addressed; findings 1 and 2 merged into one recovery mechanism after
implementation-time evidence refined the design.

- **Findings 1 + 2 — reader failure recovery (`AvfDecoder::recover_read_failure`).** On any
  reader failure, rebuild the reader with the start pulled back from where the caller was —
  just past the last emitted frame (mid-walk failure), or the requested seek start — doubling
  the back-off (1 s, 2 s, … clamped to stream start) until decode sticks, then walk forward and
  trim back up to that position (`EmitFrom`; trimmed frames skip plane-copy/surface-wrap).
  Non-reader errors (unsupported pixel format, lock failure) pass through untouched. If even
  the stream start won't decode, the error surfaces via `reset_failed_reader`, which replaces
  the dead reader (a `Failed` reader can never vend again) and clears the roll anchor so the
  next `frame_at` re-seeks on a healthy reader — the sticky-failure half of the original
  finding 2.
  - The review's "retry only when zero frames were delivered" trigger turned out to be wrong in
    practice: VideoToolbox sometimes vends a few decodable frames from a bad start point before
    dying on the leading B-frames (visible only when the recovery walk owns the trimming — the
    original failing reader trims them internally and *looks* like it failed with zero output).
    A reader started at a non-IDR sync can also fail **mid-walk** when crossing a later
    open-GOP boundary. Resuming from the caller's position on *any* reader failure handles
    every observed shape; the back-off keeps genuinely corrupt files bounded (a couple of
    decode walks per caller-visible error).
- **Regression fixture.** `tests/fixtures/opengop_h264.mp4` (200 KB, 320×180 24 fps 10 s),
  encoded with the exact x264 reference structure of the failing real-world asset
  (`open_gop=1`, `b_pyramid=2`, `weightb=1`, `ref=3` — recovered from the asset's own x264
  options SEI): sync samples every 2 s, only frame 0 IDR, leading B-frames on the 4 s and 8 s
  sync samples. Locked by three integration tests: exact recovery on the previously-failing
  seek (plus decoder health after), random access into every GOP both directions, and
  `frame_at_nearest` through the same recovery. A first fixture attempt with default x264
  reference structure did *not* reproduce (its leading Bs were backward-predicted only) — the
  reference structure is the load-bearing part.
- **Finding 3 — error diagnostics.** `reader_error` now reports the `NSError` domain + code,
  the underlying error (the VideoToolbox `OSStatus` — e.g. `-12137`, invisible before), and
  the reader's requested start / last emitted PTS:
  `read failed: Cannot Decode [AVFoundationErrorDomain -11821] (underlying: … [NSOSStatusErrorDomain -12137]) (reader start 5.000s, no frames delivered)`.
  Unit-tested via a constructed `NSError` chain.
- **Finding 4 — `frame_at_nearest` cost on this backend.** Documented in the module docs (the
  reader trims to the time range, so "seek + one decode" still pays the full GOP-prefix decode;
  a cheap snap needs the demux/decode split). No behavior change.
- **Finding 5 — VFR frame rate.** `resolve_frame_rate` keeps the exact
  minimum-frame-duration rate only while it agrees with the nominal average (±5%); a VFR
  source's smallest gap no longer inflates `frame_count`. Unit-tested.
- **Finding 6 — misc.** The probe's audio check reuses the decoder's already-parsed asset
  (`AvfDecoder::has_audio_track`, mirroring the Windows backend; the path-based helper is gone),
  and `rational_to_cmtime` falls back to microsecond resolution instead of wrapping on
  `value·den` overflow. Both unit-tested.

### Measured after the fix (same 4K open-GOP asset, M-series, release)

| seek target | before | after |
| --- | --- | --- |
| cold `seek` 30 s / 60 s / 100 s | **Cannot Decode** | 30.030 s / 60.060 s / 100.058 s in 1.2–1.4 s |
| `decode_bench` random scrub (64 uniform targets) | panic on first bad-GOP target | 64/64, 558 ms mean, 1.45 s p95 |
| sequential / roll-forward playback | 2.7 ms/frame | unchanged (2.7 ms/frame, 1.05× overhead) |

The recovery cost is the GOP-prefix walk the file's own structure demands; closed-GOP sources
never enter the path (no reader failures), and the adaptive roll window absorbs the higher
observed seek cost by rolling further before seeking. The open-GOP fixture's seek-per-frame
bench — which previously died at the 5 s boundary — now completes all 240 frames.

- Verified: `cargo test -p cutlass-decoder` (72 tests, parallel and `--test-threads=1`),
  `cargo clippy -p cutlass-decoder --all-targets` clean, `cargo check --workspace --all-targets`
  clean.
