# Renderer roadmap

Implementation plan for the **`renderer`** crate. Follows the design in **`renderer-research.md`**. Build in order — each phase ends in something **runnable and tested**.

**Scope discipline:** everything before the **MVP cutline** ships in v1. Everything after is **documented but deferred** — design is in `renderer-research.md`; phases below define the *implementation* plan when their time comes.

---

## Phase 0 — Workspace scaffold + wgpu smoke test

**Goal:** the `renderer` crate exists, wgpu links, `Renderer::new()` produces a `Device` + `Queue` on your Mac.

**Tasks:**

1. Create `crates/renderer` (lib) in the workspace.
2. Add deps to `crates/renderer/Cargo.toml`:
   - `wgpu` (latest stable — pin a specific version to avoid breaking changes mid-roadmap)
   - `pollster` (sync wrapper around async wgpu init)
   - `bytemuck` (for safe casting of uniforms / buffers, will need it later)
   - `thiserror`
   - `decoder = { path = "../decoder" }` (for `DecodedVideoFrame`, `PixelFormat`, `Plane`)
3. Write `examples/wgpu_probe.rs`:
   ```rust
   fn main() {
       let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
       let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
           power_preference: wgpu::PowerPreference::HighPerformance,
           compatible_surface: None,
           force_fallback_adapter: false,
       })).expect("adapter");
       let info = adapter.get_info();
       println!("adapter: {} ({:?}) backend={:?}", info.name, info.device_type, info.backend);
   }
   ```

**Deliverable:** `cargo run -p renderer --example wgpu_probe` prints adapter info. On your M-series Mac this should print Metal backend + Apple GPU device.

**Common gotchas:**

- wgpu version mismatch with `bytemuck` features — pin both.
- If wgpu won’t find Metal, it’s a system issue; should Just Work on macOS.

---

## Phase 1 — Core types (no GPU work yet)

**Goal:** all renderer-facing types compile and have unit tests. Zero pipelines, zero shaders.

**Modules:**

- `error` — `RendererError` enum via `thiserror`.
- `target` — `RenderTarget` struct (carries `Texture` + `TextureView` + `width` + `height`). Constructor takes `&wgpu::Device`.
- `layer` — `Layer` struct (owned `DecodedVideoFrame`), `Transform` struct + `Transform::identity()`, `opacity: f32` field on `Layer`.
- `pixel_format` (internal) — helper module mapping `decoder::PixelFormat` to wgpu texture format(s) and plane count.

**Tests:**

- `Transform::identity()` returns the expected zeros / ones.
- `PixelFormat::Yuv420p` maps to 3 planes; `Nv12` to 2; `Rgba8` to 1.
- `RendererError::Display` produces useful strings for each variant.

**Deliverable:** `cargo test -p renderer` passes with type-only unit tests. No wgpu calls in tests.

---

## Phase 2 — `Renderer::new()` + pipeline construction

**Goal:** `Renderer::new()` builds three render pipelines (YUV420P, NV12, RGBA8). Nothing renders yet, but the pipelines exist and the shaders compile.

**Tasks:**

1. `pub struct Renderer` holding:
   - `device: wgpu::Device`
   - `queue: wgpu::Queue`
   - `pipeline_yuv420p: wgpu::RenderPipeline`
   - `pipeline_nv12: wgpu::RenderPipeline`
   - `pipeline_rgba8: wgpu::RenderPipeline`
   - 3 × `bind_group_layout: wgpu::BindGroupLayout` (one per pipeline)
   - `sampler: wgpu::Sampler` (Linear filtering, ClampToEdge)
2. `Renderer::new() -> Result<Self, RendererError>`:
   - Init wgpu (instance → adapter → device + queue) via `pollster::block_on`.
   - Build three pipelines from inline WGSL strings.
   - Use the fullscreen-triangle vertex shader (see research doc).
   - **MVP shader stubs:** fragment shader just outputs solid red for now (`vec4(1.0, 0.0, 0.0, 1.0)`). Real color math arrives in Phase 4. Goal here is to prove pipeline construction.
3. Three WGSL files under `shaders/`:
   - `yuv420p.wgsl` — bindings for 3 textures + sampler, FS outputs red.
   - `nv12.wgsl` — bindings for 2 textures + sampler, FS outputs green.
   - `rgba8.wgsl` — bindings for 1 texture + sampler, FS outputs blue.
   - Different colors per pipeline so Phase 4 visually distinguishes which pipeline ran.

**Tests:**

- `renderer_new_succeeds` — `Renderer::new()` returns `Ok(_)` on this hardware.
- `renderer_is_send` — compile-time assert `Renderer: Send`.
- `renderer_is_not_sync` — compile-time assert `!Sync` (use a static check or just doc-test).

**Deliverable:** renderer constructs, three pipelines exist, shaders compile. No rendering yet.

---

## Phase 3 — Plane upload with row padding

**Goal:** given a `DecodedVideoFrame`, upload its planes to wgpu textures with correct stride padding. Verify by inspecting texture dimensions and format — no rendering yet.

**Tasks:**

1. Internal helper `upload_yuv420p_planes(&self, frame: &DecodedVideoFrame) -> [wgpu::Texture; 3]`.
2. For each plane:
   - Determine target wgpu format (`R8Unorm`).
   - Compute plane width and height per the pixel format’s sampling.
   - Compute `padded_stride = align_up(plane_width_bytes, COPY_BYTES_PER_ROW_ALIGNMENT)`.
   - Allocate a `Vec<u8>` of size `padded_stride * plane_height`; copy rows from `Plane.data` (using `Plane.stride`) into the padded buffer.
   - Create `wgpu::Texture`; `Queue::write_texture` with `ImageDataLayout { bytes_per_row: Some(padded_stride), .. }`.
3. Equivalent helper for NV12 (2 planes; second plane is `Rg8Unorm`).
4. Equivalent helper for RGBA8 (1 plane; `Rgba8Unorm`).

**Tests:**

- `upload_yuv420p_320x240_succeeds_and_textures_have_expected_size` — open `testsrc_h264.mp4` via decoder, decode one frame, upload, check the 3 returned textures have `width=320, height=240`, `width=160, height=120`, `width=160, height=120`.
- `upload_handles_non_256_stride` — synthesize a fake `DecodedVideoFrame` with `Plane.stride = 320` (not 256-aligned); upload should succeed without panicking. Verify by hash of the padded buffer (or just no panic + correct texture extent).

**Deliverable:** upload path works for all three formats. Stride padding is correct. No GPU readback yet — visual confirmation comes in Phase 4.

---

## Phase 4 — YUV420P render + readback (the big one)

**Goal:** actually render a YUV420P frame to a `RenderTarget`, read back pixels, assert correctness. **This is where color math correctness is proven.**

**Tasks:**

1. Replace the “solid red” fragment shader in `yuv420p.wgsl` with the **real BT.709 limited range** YUV→RGB conversion (see research doc shader sketch). **Cross-reference math against mpv or another reference implementation** before considering it done.
2. Implement `Renderer::render(&mut self, layers: &[Layer], target: &mut RenderTarget) -> Result<(), RendererError>`:
   - Assert `layers.len() == 1` (or return `UnsupportedLayerCount`).
   - Upload the frame’s planes (Phase 3 helpers).
   - Build a bind group from the textures + sampler.
   - Begin a render pass writing into `target.view`.
   - Pick pipeline by `frame.data` variant + `CpuFrame.format`.
   - Set bind group, draw 3 vertices (`0..3`).
   - Submit.
3. Implement `Renderer::read_pixels_rgba8(&self, target: &RenderTarget) -> Result<Vec<u8>, RendererError>`:
   - Create a `wgpu::Buffer` sized `padded_row * height`.
   - `CommandEncoder::copy_texture_to_buffer`.
   - Submit + `device.poll(Wait)`.
   - `buffer.slice(..).map_async`; wait via `device.poll(Wait)` again.
   - Strip per-row padding into a tight `width * height * 4` `Vec<u8>`.
4. **Color correctness fixture:** create a synthetic `DecodedVideoFrame` with Y=128, U=128, V=128 (gray middle). After BT.709 conversion, expected RGB ≈ `(127, 127, 127)` (within ~2 units of tolerance for the limited-range expansion).
5. **Black fixture:** Y=16, U=128, V=128 → expected RGB ≈ `(0, 0, 0)`.
6. **White fixture:** Y=235, U=128, V=128 → expected RGB ≈ `(255, 255, 255)`.

**Tests:**

- `render_yuv420p_solid_mid_gray_produces_mid_gray_rgb`
- `render_yuv420p_solid_black_produces_black_rgb`
- `render_yuv420p_solid_white_produces_white_rgb`
- `render_yuv420p_real_h264_first_frame_produces_non_zero_rgb` — open `testsrc_h264.mp4`, decode first frame, render, readback, assert the output buffer has some non-zero variance (not all one color).

**Deliverable:** YUV420P renders correctly. Color math is verified against known values. Readback works.

---

## Phase 5 — NV12 path

**Goal:** same as Phase 4 but for NV12 input.

**Tasks:**

1. Generate an NV12 test fixture:
   ```bash
   ffmpeg -f lavfi -i testsrc=duration=2:size=320x240:rate=30 \
     -c:v libx264 -pix_fmt nv12 tests/assets/testsrc_nv12.mp4
   ```
2. Write the NV12 fragment shader (same math as YUV420P, but U and V come from `.r` and `.g` of an `Rg8Unorm` texture).
3. Wire pipeline selection in `Renderer::render` to dispatch on format.

**Tests:**

- `render_nv12_solid_colors_match_yuv420p_solid_colors` — same gray/black/white synthetic inputs, same RGB outputs (within tolerance) regardless of which planar layout.
- `render_nv12_real_file_first_frame_non_zero`.

**Deliverable:** NV12 produces identical output to YUV420P for equivalent content.

---

## Phase 6 — RGBA8 path (trivial)

**Goal:** RGBA8 input renders correctly. No color conversion — just sample and output.

**Tasks:**

1. Synthesize an RGBA8 `DecodedVideoFrame` (no need for an FFmpeg fixture — just hand-build a frame in the test).
2. Fragment shader: `textureSample(t_rgba, s, in.uv)` and return it.

**Tests:**

- `render_rgba8_passes_through_unchanged` — synthetic RGBA8 input with known pixel pattern; readback matches input bit-for-bit (after row-stride normalization).

**Deliverable:** RGBA8 path works. All three input formats covered.

---

## Phase 7 — End-to-end with engine + decoder

**Goal:** wire the full pipeline: open file via engine → receive `Frame` event → build `Layer` → render → readback. One integration test that proves the whole pipeline.

**Tasks:**

1. Integration test `tests/end_to_end.rs`:
   - Add `engine` as a dev-dependency.
   - Spin up `Engine`; `Renderer`.
   - Send `Open` for `testsrc_h264.mp4`, receive `Opened`.
   - Send `SeekExact(2.0s)`, receive `Frame`.
   - Build `Layer { frame, transform: Transform::identity(), opacity: 1.0 }`.
   - Create `RenderTarget::new(&device, 320, 240)`.
   - Render. Read back. Assert non-zero variance.
2. `examples/end_to_end.rs` — same flow as a binary, prints output stats. Useful for manual smoke.

**Tests:**

- `engine_to_renderer_h264_pipeline_produces_pixels` — the integration test above.
- `engine_to_renderer_bframes_pipeline_produces_pixels` — same on `testsrc_bframes.mp4` (proves B-frame seek + render round-trip).

**Deliverable:** Cutlass has a working end-to-end media pipeline: file in, GPU-rendered RGBA bytes out. **This is the moment the whole architecture is proven.**

---

## Phase 8 — Polish: errors, docs, README, clippy

**Goal:** ship-quality library hygiene.

**Tasks:**

1. Doc comments on every public item:
   - `Renderer` — threading model (`Send`, not `Sync`), one per consumer thread.
   - `RenderTarget` — caller-owned, sized to source in MVP.
   - `Layer` and `Transform` — identity defaults; MVP ignores non-identity values.
   - `render` — MVP single-layer constraint, what `UnsupportedLayerCount` means.
2. `crates/renderer/README.md` — one-screen overview, quickstart, link to `renderer-research.md`.
3. `cargo clippy -p renderer --all-targets -- -D warnings` clean.
4. `cargo doc -p renderer --no-deps --open` renders coherently.
5. Audit every `?` and `.unwrap()` outside tests — replace with the right `RendererError` variant or document why the unwrap is sound.

**Deliverable:** all green: `cargo test -p renderer`, `cargo clippy -p renderer`, `cargo doc -p renderer`.

---

## 🚧 MVP cutline — renderer ships here

Everything below is **documented** in `renderer-research.md`. Implementation comes **after** the renderer is proven in anger (integrated with engine end-to-end, eventually wired to Slint). Don’t pre-build them.

---

## Phase 9 — Texture upload cache *(post-MVP)*

**Goal:** repeat uploads of the same frame are free.

**Tasks:**

1. Cache keyed by frame identity (TBD: stable hash, or `(SourceId, Rational pts)` passed in via `Layer`).
2. Bytes-budgeted LRU (default e.g. 128 MB GPU memory).
3. Insert after successful upload; lookup before upload path.
4. Invalidate on source-size change or explicit `Renderer::clear_cache()` (for tests / memory pressure).

---

## Phase 10 — Multi-layer composite *(post-MVP)*

**Goal:** `render(&[Layer], ...)` actually composites N layers in order.

**Tasks:**

1. Drop the `layers.len() == 1` guard.
2. Per-layer transform uniform; vertex shader applies it.
3. Alpha blending state for `opacity < 1.0`.
4. RenderTarget gets a configurable clear color (currently transparent / black; multi-layer wants explicit clear).
5. Draw layers back-to-front in slice order.

**Tests:**

- Two solid-color layers; bottom red, top half-opacity green; expected blend.
- Two real frames side-by-side via transforms.

---

## Phase 11 — Per-layer transform application *(post-MVP)*

**Goal:** `Transform` fields actually affect output.

**Tasks:**

1. Pack `Transform` into a uniform buffer per draw call.
2. Vertex shader applies translate / scale / rotate before mapping to clip space.
3. RenderTarget canvas size becomes independent of source size.

---

## Phase 12 — Color metadata from decoder *(post-MVP)*

**Goal:** correctly render BT.601, BT.2020, full-range sources.

**Tasks:**

1. Engine extension: surface `color_primaries`, `color_trc`, `color_space`, `color_range` from `AVCodecContext`. Add `ColorInfo` to `DecodedVideoFrame`.
2. Renderer reads `ColorInfo` per frame; selects matrix + range expansion via uniform (not separate pipelines — too many variants).
3. Test fixtures for BT.601 SD content and full-range sources.

---

## Phase 13 — Filter graph / effects *(post-MVP)*

Out of scope for now. When the time comes:

- `Layer` carries an effect chain.
- Renderer runs each layer through its effects into an intermediate texture, then composites.
- Effects are user-extensible (trait-based) or built-in registry.

---

## Phase 14 — Slint texture interop *(post-MVP, after UI exists)*

**Goal:** `RenderTarget` texture is consumed by Slint without a CPU round-trip.

**Tasks:**

1. Use Slint’s `wgpu` texture sharing API (the exact API name will depend on the Slint version at the time).
2. `RenderTarget::for_slint(slint_window) -> RenderTarget` constructor.
3. Verify zero-copy path on macOS Metal backend.

---

## Test asset reference

Renderer integration tests reuse the **decoder’s** fixtures. Engine roadmap already established the `regenerate.sh` pattern — renderer should mirror that for any renderer-specific fixtures (e.g. NV12).

| File | Used in phase | Purpose |
|---|---|---|
| `testsrc_h264.mp4` | 4, 7 | YUV420P input through full pipeline. |
| `testsrc_bframes.mp4` | 7 | Proves B-frame seek correctness survives render. |
| `testsrc_nv12.mp4` | 5 | NV12 input through pipeline. |

Synthetic frames (hand-built, not file-based) cover the color-math correctness tests in Phase 4 and the RGBA8 path in Phase 6.

---

## Order-of-operations rule

**Don’t parallelize phases.** Phase 4 (real YUV→RGB shader + readback) is the hard one — if upload (Phase 3) is broken, Phase 4 will look like a shader bug. Each phase’s tests must be **green** before moving on. Same rule as decoder and engine.

## Out of scope for renderer *(forever, or until proven needed)*

- Windowed / surface-based rendering (consumer’s problem; Slint does this).
- Audio waveform rendering (different concern; if it exists, it’s a separate crate).
- Video export encoding (use FFmpeg via a separate `export` crate; renderer’s job ends at RGBA8 in memory).
- Hardware-decoded GPU surfaces (waiting on decoder’s `FrameData::Gpu` variant; renderer would consume those directly when they exist).
- 3D / mesh / non-quad geometry (the renderer is a 2D compositor; if we ever need 3D, that’s a different renderer).