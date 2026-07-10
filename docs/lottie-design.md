# Lottie support — decoder, frame strategy, asset model

**Status (macos-dev, Jul 2026):** design locked, implementation landing with
this doc. Lottie is for **stickers and overlays** (Workstream 8 of the cloud
roadmap): vector animations downloaded from the asset catalog and placed on
sticker lanes. Lottie-based *text* stays off the table — Lottie text layers
aren't content-editable in practice; animated titles are native text presets
([cloud-roadmap.md](cloud-roadmap.md)).

## Decoder backend: velato + vello_cpu

Candidates considered:

| Backend | Nature | Verdict |
|---------|--------|---------|
| **velato** (linebender) | pure Rust, parses Lottie → draw calls via a `RenderSink` trait | **picked** |
| dotlottie-rs (LottieFiles) | Rust wrapper over vendored ThorVG (C++) | rejected |
| rlottie bindings | battle-tested C++, stagnant upstream | rejected |

Rationale:

- **Pure Rust wins on build and portability.** dotlottie-rs vendors ThorVG:
  a C++ toolchain plus `bindgen`/libclang on every build host, for every
  target we ship (macOS, Windows, Linux, iOS, Android). velato +
  `vello_cpu` is `cargo add` on all of them. rlottie is unmaintained
  upstream and has the same C++ cost.
- **Rendering coverage is acceptable for stickers.** velato's documented
  gaps (text, time remapping, `ti`/`to` position easing, stroke dash,
  motion blur, split rotations) hurt full After Effects exports, not the
  simple looping stickers we curate. The catalog is first-party curated
  (CC0 in, CC0 out), so every shipped asset is validated against the real
  decoder by a drift test before it ships — coverage gaps become curation
  rejects, not user-visible breakage.
- **`vello_cpu` as the raster backend.** velato's optional `vello`
  integration drags in wgpu; our compositor already owns the GPU. Instead
  we implement velato's `RenderSink` (4 required methods) over
  `vello_cpu::RenderContext` — verified end-to-end (fills, strokes, clip
  layers, gradients render correctly to RGBA pixmaps). `vello_cpu` is
  SIMD-optimized and fast enough for on-demand 512 px frames; kurbo/peniko
  versions unify across both crates.
- **Known sharp edge: velato's importer panics** (`todo!()`) on some
  unsupported features (observed: split rotation) rather than returning
  `Err`. Parsing therefore runs under `catch_unwind`, mapping panics to
  `DecodeError` — a hostile or over-fancy file yields a failed load, never
  a crashed editor. The curation drift test catches these before shipping.

The module lives in `cutlass-decoder` (`lottie.rs`), mirroring the
animated-sticker decode module: bytes/path in, `RgbaImage` frames out.
velato and `vello_cpu` are portable pure-Rust deps compiled on every
platform.

## Frame strategy: capped-fps sampling, render-on-demand, LRU

The sticker pipeline (decode **all** frames up front, 256-frame / 1024 px
caps) is wrong for Lottie and is not reused. A 10 s, 30 fps, 512 px
animation pre-rendered is ~300 MB of RGBA; Lottie files are also
arbitrarily long. Policy instead:

- **Sample at a capped fps.** Frames are sampled at
  `min(composition fps, 20)`. Requested times quantize to that grid, so a
  60 fps timeline scrubbing over a Lottie clip hits at most 20 distinct
  frames per second of animation — cache hits, not re-renders. 20 fps is
  visually fine for decorative stickers (hand-tuned GIF stickers ship at
  ~12 fps).
- **Cap render resolution.** Frames rasterize at the composition's
  intrinsic size capped to **512 px** on the long side (`LOTTIE_MAX_DIM`).
  Vector data upsamples cleanly only to a point; 512 px covers sticker
  placements on a 1080p canvas at typical scales, and the compositor
  samples it like any bitmap. (Placement-aware resolution was considered
  and rejected: it makes the cache key depend on zoom/scale and thrashes
  on every transform tweak.)
- **Render on demand behind a per-asset LRU.** Nothing pre-renders. Each
  loaded animation keeps an LRU of rasterized frames with a byte budget
  (`LOTTIE_CACHE_BYTES` = 32 MB per asset); at 512 px (1 MB/frame) that's
  ~32 frames — a full loop of a typical 1.5 s sticker, so steady-state
  playback costs zero rasterization. Frames used by the scene currently
  being composed are never evicted (scene-stamped), so multi-clip frames
  can't alias mid-frame.
- **Duration/looping** follow sticker semantics: the animation loops over
  its intrinsic duration for the life of the clip.

## Asset model: file-backed `Generator::Lottie`

`Generator::Sticker { asset }` references the compile-time embedded
catalog — that can't work for downloaded content. Lottie assets are
**file-backed, path-referenced like media**:

```rust
Generator::Lottie {
    /// Absolute path to the .json on disk (downloaded into the cloud
    /// asset cache, or user-imported).
    path: String,
    /// Intrinsic composition size, captured at import time so the
    /// resolver stays pure (no I/O at resolve time).
    width: u32,
    height: u32,
}
```

- **Placement convention** matches stickers: intrinsic pixels are
  *reference pixels* (1080p canvas), so a 256 px Lottie lands at a
  CapCut-like overlay size instead of aspect-fitting to the canvas.
- **Size at import, not resolve.** The scene resolver is pure and
  synchronous; it cannot open files. Width/height are probed once when
  the generator is created (Library drop) and stored in the project. The
  renderer re-probes lazily and trusts its own numbers for rasterization;
  the stored size only drives placement.
- **Missing files degrade, never fail.** Like an unknown sticker id, a
  Lottie whose file is gone (cache cleared, project moved machines)
  renders nothing and logs; the clip stays editable. This is the media
  offline-relink story, not an error.
- **Serialization** is additive: a new externally-tagged variant next to
  `Sticker`, mirrored in the hand-written back-compat deserializer. Old
  builds refuse projects using it (unknown variant) — the standard
  additive-format position, surfaced by the app-update nudge.
- Lane rules: `Generator::Lottie` lands on **sticker lanes**, exactly like
  `Sticker`/`Shape`.

## Distribution

- Catalog: `GET /v1/assets/lottie` — the same file-driven
  `CatalogResponse` the other asset kinds use ([backend
  catalog](../../cutlass-backend/docs/ARCHITECTURE.md)); files and preview
  thumbnails on the CDN, metadata only through the backend, anonymous +
  rate-limited + ETag-cached.
- Client: `cutlass-cloud` gets `lottie()` beside `sfx()`/`luts()`;
  downloads go through the existing quota-managed asset cache
  (`cutlass_cloud::download`).
- Desktop: Library > Stickers grows a **Lottie** section (catalog browse,
  eager download of the small JSON files, thumbnail = frame 0, drop =
  `Generator::Lottie`).
- Licensing: CC0 only, provenance manifest per asset, per the catalog
  content policy.
