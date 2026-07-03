# Review: `cutlass-text` (standalone)

- **Date:** 2026-07-03
- **Branch:** `mobile-support` at `dbcd1de`
- **Scope:** The whole crate (`Cargo.toml`, `src/lib.rs`, `src/style.rs`, `tests/compose.rs`),
  reviewed standalone (not against `main` — the crate is new), plus how `cutlass-render`
  consumes it. `cargo check -p cutlass-text` passes.

## Findings

1. **Bug: `align` + `max_width` clips glyphs out of the bitmap** (`src/lib.rs`, measurement loop
   near line 89). The bitmap is sized from `run.line_w`, which is the sum of glyph advances and
   does **not** include the alignment offset — but glyph `x` positions **do** include it
   (cosmic-text bakes `(line_width - visual_line.w) / 2` into every glyph for `Align::Center`,
   computed against the buffer width). Example: `with_align(Center).with_max_width(400.0)` on a
   200px line puts glyphs at x ∈ [100, 300] in a 200px-wide bitmap — the right half is silently
   dropped by the draw callback's bounds check, the left half stays empty.
   **Fix (covers finding 2 as well):** measure actual glyph extents (`min_x..max_x` across runs)
   and translate by `-min_x` when drawing, keeping the bitmap tight regardless of alignment.
2. **Surprise: `align` without `max_width` is a silent no-op.** With buffer width `None`,
   cosmic-text computes the alignment correction against each paragraph's own width (offset = 0),
   and paragraphs (split by `\n`) are laid out independently — so a two-line title with
   `with_align(Center)` comes out left-flush. This is the only configuration the engine currently
   uses: `cutlass-render`'s `map_text_style` deliberately passes no wrap width (see the
   `resolve.rs` comment), so the `align` field does nothing on the real call path today. Either
   implement cross-paragraph alignment (size against the widest paragraph and offset the rest) or
   document that `align` requires `max_width`.
3. **Performance: the renderer re-shapes identical text every frame** (`cutlass-render`
   `src/render.rs`, `LayerSource::Text` arm near line 122). `SwashCache` amortizes glyph
   rasterization, but shaping, layout, the bitmap allocation, and the per-pixel blend rerun per
   frame during export/scrub — per-frame hot-path work under the workspace perf rule, for input
   that changes rarely. A `(content, style) → RgbaImage` memo in `Renderer` turns a 300-frame
   export with one title from 300 shapings into 1. (Consumer-side fix, noted here because the
   crate's API shape invites it.)
4. **Doc contract vs. behavior on empty/whitespace input.** The `rasterize` doc says
   whitespace-only text "yields a zero-area image", but spaces have advances (`line_w > 0`), so
   `" "` produces a non-zero fully transparent bitmap — and because padding is added before the
   zero-size check, `padding > 0` makes even truly empty text produce a `2·pad`-square blank
   image. Early-return when there are no layout runs (or no coverage) before applying padding.
5. **`load_font` cannot report failure.** `fontdb::load_font_data` silently drops unparseable
   data, so a corrupted bundled font degrades to "wrong glyphs at runtime". Returning the
   face-count delta (or asserting it in debug builds) catches it at load time.
6. **Fontless CI silently skips nearly all tests.** Four of five unit tests and the integration
   test early-return when `font_count() == 0`, so a headless CI box reports green while
   exercising nothing. The crate already advertises `load_font` for deterministic output —
   bundling a tiny OFL-licensed subset font as a test fixture makes the tests deterministic
   everywhere and deletes the skip branches.
7. Minor: `over_straight` has two unreachable defensive branches (`sa <= 0.0` and
   `out_a <= 0.0` — the caller filters zero alpha, and `out_a ≥ sa > 0`).

## Strengths

- **Layering is exactly right:** pure CPU, depends only on `cosmic_text` + `cutlass_core`, no
  `wgpu` knowledge; the GPU handoff lives in the renderer, and the doc header states the
  boundary explicitly.
- **Blend math is correct** straight-alpha source-over with proper rounded division
  (`(coverage * alpha + 127) / 255`); blending rather than overwriting handles overlapping
  glyphs (italic overhang, combining marks); the opaque fast path is valid for source-over.
- Using the draw callback's `color.r/g/b` (instead of assuming the fill color) makes color-emoji
  glyphs work for free.
- `Shaping::Advanced` is the right default for non-Latin scripts.
- The integration test is a genuine vertical slice: rasterize → composite on a real GPU → read
  back, asserting both coverage and color neutrality, with sensible headless/fontless skips.
- `Cargo.toml` documents the cosmic-text 0.17 MSRV pin rationale.

## Verdict

Clean crate with the right boundaries. Finding 1 is the one to fix before text ships anywhere
user-visible; findings 2–3 determine whether `align` and export performance behave as users
expect.

## Addendum (2026-07-03): resolutions + `ShapedText` API

Same-day follow-up: the measurement/rasterization path was rebuilt around a new two-phase
`shape()` API (`ShapedText` / `ClusterBox`), added to support character-level text animations
(typewriter, per-char fade, wave). The full string is shaped once — kerning, ligatures, BiDi,
complex scripts stay correct — and each shaping cluster is rasterized into its own positioned,
ink-tight bitmap (byte range, line index, offset, baseline). `rasterize()` is now a thin
compositor over `shape()`, so both entry points share one measurement path. Whitespace clusters
are kept (zero-area image) so stagger timing counts them as beats; clusters sort in logical text
order (typewriter order, correct for RTL). Compositing clusters per frame needs no compositor
changes (`LayerPlacement` already carries per-layer transform + opacity).

Status of the findings above:

- **Finding 1 (align + `max_width` clipping): fixed.** Measurement now uses real glyph ink
  extents (union of swash image boxes), so alignment offsets baked into glyph positions
  normalize out. Regression test: `center_align_with_wrap_width_does_not_clip`.
- **Finding 2 (align without `max_width` no-op): fixed.** When `align != Left` and no wrap width
  is set, layout runs a second pass with the buffer width set to the widest measured line
  (+1px slack, trimmed back off by ink-extent measurement), so lines align against each other.
  Test: `center_align_without_wrap_width_centers_short_lines`.
- **Finding 3 (per-frame re-shaping): fixed at the crate level.** `TextRenderer` now memoizes
  both `shape` and `rasterize` results keyed by (text, style) — f32 fields keyed by bit pattern,
  bounded by a clear-at-64-entries policy, invalidated by `load_font` (a new face can change
  shaping for any string). Per-frame callers now pay a memo lookup plus one bitmap copy instead
  of shape + layout + blend; `cutlass-render` needs no changes to benefit. Test:
  `memo_caches_repeat_calls_and_invalidate_on_font_load`. (A renderer-side per-cluster
  animation path is still future work, but no longer a perf prerequisite.)
- **Finding 4 (whitespace/padding phantom bitmaps): fixed.** Zero-coverage runs return a
  zero-area image before padding is applied. Test: `whitespace_is_zero_area_even_with_padding`
  (runs even on fontless CI). Note the bitmap is now ink-tight (space advances no longer grow
  it), which also means the layer placement centers the *visible* glyphs.
- **Finding 5 (`load_font` silent failure): fixed.** `load_font` returns the number of faces
  actually added (`0` = unparseable bytes), the only failure signal `fontdb` allows. Existing
  callers that ignore the return keep compiling. Test: `load_font_reports_added_faces`.
- **Finding 6 (fontless CI skips): fixed.** A bundled OFL face (`assets/Micro5-Regular.ttf`,
  53 KB, license alongside as `assets/OFL.txt`) is loaded by every glyph-producing test via a
  `test_renderer()` helper, so the suite asserts real shaping everywhere — no more
  skip-and-report-green. The only remaining skip is the GPU-adapter check in the compose
  integration test, which is genuinely environmental.
- **Finding 7 (`over_straight` unreachable branches): fixed** — dead guards removed; the
  non-zero-source precondition is documented and enforced by callers.
