# Cutlass Roadmap

Ordered list of upcoming work. Each item should be small enough to land as one
reviewable change (or a short series). Update this file as items complete.

## Already landed (context, not work)

- Timeline → MP4 export, end to end: per-frame GPU composite + H.264/AAC mux
  (`crates/cutlass-render/src/export.rs`), wired into the engine
  (`Command::Project(Export)`), the desktop export job
  (`apps/cutlass-desktop/src/preview_worker.rs`), and mobile
  (`crates/cutlass-mobile/src/export_job.rs`).
- `cutlass-py`: functional v2 track-first API (`Project`, `Track`, `Clip`,
  `get_frame() -> numpy`, `export()`), with a passing integration suite.
- Param/keyframe system (`crates/cutlass-models/src/param.rs`) including speed
  curves; Phase I "look" data model (`crates/cutlass-models/src/look.rs`)
  persisted + validated but render-neutral.
- Compositor pipeline benchmark over real media
  (`crates/cutlass-render/examples/composite_bench.rs`) — use it to guard GPU
  cost regressions for the render work below.

## 1. Render clip color adjustments and filter presets — DONE

Per-clip `clip.adjust` (`ColorAdjustments`) and `clip.filter` (preset id ×
intensity) now grade pixels in preview and export.

- `ColorGrade` on `CompositeLayer` (`crates/cutlass-compositor/src/layer.rs`),
  applied in all four fragment shaders via the shared
  `crates/cutlass-compositor/shaders/grade.wgsl`.
- Preset id → recipe table and `effective_grade()` in
  `crates/cutlass-render/src/grade.rs`; threaded through
  `resolve_clip` → `SceneLayer.grade` → `Realized` → `with_grade()`.
- Lane-level `Generator::Filter`/`Adjustment` bars still skipped (item 4).
- Benchmarked with `composite_bench`: compositor timings within run noise.

## 2. Render masks and chroma key

Next slice of the look system: per-clip alpha work.

- Mask shapes (rect/ellipse/linear/mirror/…) as an alpha pass; chroma key as a
  keying pass with the persisted intensity/shadow params.
- Same files as item 1.

## 3. Clip effects and transitions (M4 GPU passes)

Model, catalogs, Python API, and drift tests exist; the compositor has no
effect infrastructure yet — this is greenfield GPU work.

- Add effect descriptors + WGSL passes to `cutlass-compositor`; hook clip
  effects (`crates/cutlass-models/src/effects.rs`) and transitions
  (`crates/cutlass-models/src/transition.rs`) into the render graph.
- The deferred test suite in `crates/cutlass-engine/tests/deferred/` sketches
  the intended API — revive what still makes sense.

## 4. Lane-level generators: Effect / Filter / Adjustment

Remove the explicit skip in `crates/cutlass-render/src/resolve.rs`
(`Generator::Effect | Generator::Filter | Generator::Adjustment => None`) by
applying the passes from items 1–3 to everything beneath the lane instead of a
single clip. Desktop UI already projects phantom lanes
(`apps/cutlass-desktop/src/projection.rs`).

## 5. Stickers

Last skipped generator kind. Needs asset handling (animated sticker sources)
plus compositing as `Rgba`/`Frame` layers.

## 6. Look animations (entrance / exit / combo)

Drive the persisted animation catalogs from `look.rs` through the param system
at resolve time (transform/opacity over the clip's local timeline).

## 7. Export audio: retimed and denoised clips

Video retimes via speed curves, but affected clips currently export **silent**
(`crates/cutlass-render/src/export_audio.rs`). Implement varispeed resampling
for speed-ramped audio and run RNNoise in the export mix.

## 8. cutlass-py polish

- Allow still-image `import_media` (renderer already supports
  `LayerSource::Still`; the Python side may still reject PNGs).
- Sync README with current capabilities; optional PyPI packaging via maturin.

## 9. Non-Apple export backends

`crates/cutlass-encoder` returns `Unsupported` on Linux/Windows. Add a backend
(e.g. FFmpeg libx264 or platform encoders) if cross-platform export matters.

## 10. Docs debt

- `.cursor/rules/overview.mdc` still claims the export pieces "aren't joined";
  update it (and the `CompositeLayer` gap note) as items above land.
