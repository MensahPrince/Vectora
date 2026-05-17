# Renderer research

The **renderer** crate takes `DecodedVideoFrame`s from the engine and produces RGBA pixel images on the GPU via **wgpu**. It is the layer between decoded YUV planes and anything that needs an RGB image (Slint preview, file export, screenshot tests).

This doc covers the **full vision**. The **MVP cutline** is explicit at the bottom and noted per-section.

---

## Mission and boundaries

**Renderer owns:**

- A `wgpu::Device` + `wgpu::Queue` (offscreen, no `Surface`).
- Render pipelines (one per input pixel format).
- Sampler, bind group layouts, shaders.
- Per-format **YUV→RGB conversion** via fragment shader.
- A composite-aware **`Layer`** model (single layer in MVP, multi-layer is future).
- *(Future)* texture upload cache.
- *(Future)* filter graph / effects.
- *(Future)* Slint shared-texture interop.

**Renderer does NOT own:**

- The **wgpu surface or window.** Output is a `wgpu::Texture` (RGBA8); consumer decides what to do with it.
- The **frame source.** Engine provides frames; renderer takes them as inputs.
- The **timeline.** Layer ordering / clip selection happens in the consumer.
- **Audio.** Not even close to renderer.
- **HDR.** Out of scope until v2+.

**Output target stance:** the renderer is **offscreen-only** in v1. It writes into a caller-owned `RenderTarget` (a `wgpu::Texture` wrapper). Slint interop is a future *consumer* problem, not a renderer redesign — same texture, different downstream user.

**Async stance:** the wgpu adapter/device init APIs are async. Renderer wraps them with `pollster::block_on` internally and exposes a **sync `Renderer::new()`**. Consumers don’t need an async runtime.

---

## wgpu setup (offscreen)

Standard wgpu init sequence with **no surface**:

1. `wgpu::Instance::new(...)` — backend defaults are fine (Metal on macOS).
2. `instance.request_adapter(...)` — `PowerPreference::HighPerformance`, no `compatible_surface`.
3. `adapter.request_device(...)` — request the features and limits we need.
4. Hold onto `Device` + `Queue` for the renderer’s lifetime.

**Features:** none required for MVP. RGBA8 + R8Unorm + Rg8Unorm are all in `Features::empty()`.

**Limits:** default limits cover the MVP. Source dimensions up to 8192×8192 are well within `max_texture_dimension_2d`.

**No surface, no swapchain.** All output is `Queue::write_texture` → render pass → `wgpu::Texture`. Consumer reads the texture however they want.

---

## The YUV upload problem

This is **the** technical landmine. Two issues stack on top of each other:

### Issue 1: plane layout depends on pixel format

| `PixelFormat` | Planes | wgpu format per plane | Plane sizes |
|---|---|---|---|
| `Yuv420p` | 3 | `R8Unorm` × 3 | Y full; U, V at `H/2 × W/2` |
| `Nv12` | 2 | `R8Unorm` + `Rg8Unorm` | Y full; UV interleaved at `H/2 × W` (treat as `Rg8Unorm` at `H/2 × W/2`) |
| `Rgba8` | 1 | `Rgba8Unorm` | One plane full size |

### Issue 2: wgpu requires `bytes_per_row` aligned to `COPY_BYTES_PER_ROW_ALIGNMENT` (256)

FFmpeg strides are typically aligned to **16 / 32 / 64** — almost never 256. So `Queue::write_texture` with the decoded buffer **directly** will reject the layout.

**Solution: row-padding staging buffer.**

For each plane:

1. Compute `padded_stride = align_up(plane_width_bytes, 256)` (where `plane_width_bytes` accounts for the wgpu format’s bytes-per-pixel).
2. Allocate `Vec<u8>` sized `padded_stride * plane_height`.
3. Copy row-by-row from the decoded `Plane.data` (using `Plane.stride`) into the padded buffer (using `padded_stride`).
4. `Queue::write_texture` with `bytes_per_row: Some(padded_stride)`.

**Performance note:** this is an extra CPU copy per plane per frame. For 1080p that’s ~3 MB of memcpy per frame — not a bottleneck, but **measure** before claiming it’s fine on 4K. A `Buffer`-based zero-copy path (`copy_buffer_to_texture` with a pre-padded buffer maintained per-source) is a v1.1 optimization.

**Sketch (names TBD):**

```rust
fn upload_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    plane: &Plane,
    plane_width_bytes: u32,
    plane_height: u32,
) {
    let padded_stride = align_up(plane_width_bytes, 256);
    let mut padded = vec![0u8; (padded_stride * plane_height) as usize];
    for row in 0..plane_height as usize {
        let src = &plane.data[row * plane.stride..row * plane.stride + plane_width_bytes as usize];
        let dst = &mut padded[row * padded_stride as usize..][..plane_width_bytes as usize];
        dst.copy_from_slice(src);
    }
    queue.write_texture(
        texture.as_image_copy(),
        &padded,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(padded_stride),
            rows_per_image: Some(plane_height),
        },
        wgpu::Extent3d { width: plane_width, height: plane_height, depth_or_array_layers: 1 },
    );
}
```

---

## Color pipeline: BT.709 limited range (hardcoded in MVP)

The decoder currently doesn’t surface color metadata (color_primaries, color_trc, color_space, color_range from `AVCodecContext`). The MVP renderer **assumes BT.709 limited range** for all YUV input. This matches:

- H.264 / HEVC sources from typical capture devices and screen-recorded content.
- What FFmpeg labels `YUV420P` (vs `YUVJ420P` for full-range — not supported in v1).

**Why hardcode in MVP:**

- Engine doesn’t expose the metadata yet — surfacing it is a separate piece of work.
- 95%+ of MVP test material is BT.709.
- Wrong color shows up immediately on the very first end-to-end test, so we’ll know if assumption is broken.

**Shader math (BT.709 limited):**

- Normalize Y from `[16, 235]` to `[0, 1]`; U, V from `[16, 240]` to `[-0.5, 0.5]`.
- Apply BT.709 matrix:
  ```
  R = Y' + 1.5748 * V'
  G = Y' - 0.1873 * U' - 0.4681 * V'
  B = Y' + 1.8556 * U'
  ```
- Coefficients per ITU-R BT.709. **Cross-reference against a battle-tested implementation** (e.g. mpv’s `video_shaders.c`) before shipping — small constants drift in different references.

**Future:** read color metadata from decoder, support **BT.601** (older SD content), **BT.2020** (UHD), **full range**, and eventually **HDR transfer functions** (PQ, HLG). Each is a shader variant or a uniform-fed matrix.

---

## Fragment shader sketch (WGSL)

One pipeline (and one shader) per input format. All produce RGBA8 output.

```wgsl
// yuv420p.wgsl — pseudocode, exact math TBD
struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var t_y: texture_2d<f32>;
@group(0) @binding(1) var t_u: texture_2d<f32>;
@group(0) @binding(2) var t_v: texture_2d<f32>;
@group(0) @binding(3) var s_linear: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let y = textureSample(t_y, s_linear, in.uv).r;
    let u = textureSample(t_u, s_linear, in.uv).r;
    let v = textureSample(t_v, s_linear, in.uv).r;

    // BT.709 limited range conversion
    let yp = (y - 16.0/255.0) * 255.0/219.0;
    let up = (u - 128.0/255.0) * 255.0/224.0;
    let vp = (v - 128.0/255.0) * 255.0/224.0;

    let r = yp + 1.5748 * vp;
    let g = yp - 0.1873 * up - 0.4681 * vp;
    let b = yp + 1.8556 * up;

    return vec4<f32>(clamp(vec3(r, g, b), vec3(0.0), vec3(1.0)), 1.0);
}
```

NV12 uses the same math but samples U and V from the `.r` and `.g` channels of a single `Rg8Unorm` texture. RGBA8 skips the conversion entirely.

---

## Vertex pipeline: fullscreen triangle trick

Instead of a 6-vertex quad with a vertex buffer, use a **single 3-vertex triangle** that covers the screen, with positions computed from `vertex_index`. No vertex buffer needed.

```wgsl
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    let x = f32((idx << 1u) & 2u);
    let y = f32(idx & 2u);
    var out: VertexOutput;
    out.uv = vec2(x, 1.0 - y);  // flip Y for image space
    out.pos = vec4(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
    return out;
}
```

Cleaner, faster, one less thing to bind.

**Multi-layer future:** when compositing multiple layers, each layer gets its own draw call with the same vertex shader but a per-layer transform uniform. Single triangle still works; UVs and positions get transformed.

---

## `RenderTarget` and output

The renderer does **not own** its output texture. The caller creates a `RenderTarget` and passes it in. This lets the caller:

- Render to multiple targets (e.g. preview at 720p + export at 1080p).
- Hand the texture to Slint (future) without ownership wrangling.
- Read pixels back for tests.

**Sketch:**

```rust
pub struct RenderTarget {
    pub width: u32,
    pub height: u32,
    pub texture: wgpu::Texture,        // RGBA8Unorm
    pub view: wgpu::TextureView,       // cached view for color attachment
}

impl RenderTarget {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self;
}
```

**MVP target dimensions:** match source dimensions (single layer, identity transform). Future multi-layer: target is a fixed canvas (e.g. 1920×1080), layers position within it.

---

## Layer model: composite-aware, single in MVP

The renderer API takes a **slice of `Layer`** — even though MVP only ever passes one. This is the “**design for composite, ship single**” pattern.

```rust
pub struct Layer {
    pub frame: DecodedVideoFrame,    // owned; v1.1 may switch to Arc for sharing
    pub transform: Transform,         // Transform::identity() in MVP
    pub opacity: f32,                 // 1.0 in MVP
}

pub struct Transform {
    pub translate: [f32; 2],         // pixels in target space
    pub scale: [f32; 2],             // 1.0 = source size
    pub rotate_radians: f32,
}

impl Transform {
    pub fn identity() -> Self {
        Self { translate: [0.0, 0.0], scale: [1.0, 1.0], rotate_radians: 0.0 }
    }
}
```

**MVP renderer behavior:**

- Asserts `layers.len() == 1` (panic or `Err(RendererError::UnsupportedLayerCount)` — TBD).
- Ignores `transform` and `opacity` (or asserts they’re identity / 1.0).
- Renders the single layer’s frame full-target.

**Future renderer behavior:**

- Iterates layers in order, draws each into the same target with its transform and opacity.
- Blending via standard alpha over.
- Filter graph extension: each layer can have a chain of effects applied before composition.

**Why this matters now:** the API takes `&[Layer]`, not `&DecodedVideoFrame`. Adding multi-layer support later is purely an *implementation* change — no caller breaks.

---

## Pipelines: one per format

Three render pipelines in MVP, created once at `Renderer::new()`:

| Pipeline | Input bindings | Vertex / fragment |
|---|---|---|
| `yuv420p` | 3× `texture_2d<f32>` (R8Unorm) + sampler | Same VS, color-convert FS |
| `nv12` | 1× R8Unorm (Y) + 1× Rg8Unorm (UV) + sampler | Same VS, color-convert FS |
| `rgba8` | 1× Rgba8Unorm + sampler | Same VS, passthrough FS |

Bind group layouts differ; bind groups created per-frame (or cached per-source — v1.1 optimization).

**Pipeline selection** is a `match` on the input `PixelFormat`. Unsupported → `RendererError::UnsupportedFormat`.

---

## Resource lifecycle and caching

**Created once:**

- `Device`, `Queue`
- 3 × `RenderPipeline`
- 3 × `BindGroupLayout`
- 1 × `Sampler` (linear filtering, ClampToEdge)
- Shader modules

**Created or reused per frame:**

- Source textures (one per plane). **Cache** keyed by `(format, width, height)` so back-to-back frames of the same source reuse textures. Invalidate on size change.
- Bind groups. Recreate per frame in MVP (cheap); cache per-source in v1.1.
- Padded upload buffers. `Vec<u8>` reused across frames if size unchanged.

**Caller-managed:**

- `RenderTarget` (caller creates, holds, drops).

**MVP cache strategy:** keep textures alive between renders if the source dimensions are unchanged. Recreate on size change. No size-aware LRU yet — simple replace-on-mismatch.

---

## Readback for tests

Headless tests need to verify rendered pixels match expectations. The flow:

1. Create a `wgpu::Buffer` sized for the target (`width × height × 4`, rows padded to 256 — yes, the alignment problem reappears for readback).
2. `CommandEncoder::copy_texture_to_buffer` from `RenderTarget.texture` to the buffer.
3. `Queue::submit` + `Device::poll(Wait)`.
4. `Buffer::map_async` for read; await completion.
5. Copy buffer contents to a `Vec<u8>`, stripping the per-row padding to produce a tight `width × height × 4` RGBA8 buffer.

**Sketch:**

```rust
pub fn read_pixels_rgba8(&self, target: &RenderTarget) -> Result<Vec<u8>, RendererError>;
```

This method is **test-only** in spirit but lives in the public API — it’s also useful for the future export pipeline (write rendered frames to disk).

---

## Threading and Send/Sync

- `wgpu::Device` and `wgpu::Queue` are `Send + Sync`. Multiple consumers could share them — but the renderer also holds mutable state (texture caches, padded buffer reuse).
- **Decision:** `Renderer` is `Send`, **not `Sync`**. One renderer per consumer thread. If multiple consumers need rendering, give each its own `Renderer` (or share the `Device` + `Queue` and build separate renderers around them).

**MVP threading shape:** the test harness drives renderer on its own thread. Engine events arrive via channel from the engine worker. Renderer pulls events, builds a single-layer `Layer`, renders to a `RenderTarget`, reads back if needed.

---

## Error model

```rust
#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    #[error("failed to acquire wgpu adapter (no compatible GPU)")]
    NoAdapter,

    #[error("wgpu device request failed")]
    DeviceRequest(#[source] wgpu::RequestDeviceError),

    #[error("unsupported pixel format for rendering: {0:?}")]
    UnsupportedFormat(PixelFormat),

    #[error("unsupported layer count for MVP: {count} (expected 1)")]
    UnsupportedLayerCount { count: usize },

    #[error("zero-dimension frame or target")]
    ZeroDimension,

    #[error("readback failed")]
    Readback,
}
```

Renderer does **not** wrap `DecoderError` — the renderer doesn’t talk to the decoder; it talks to whatever hands it a `DecodedVideoFrame`. Consumers translate decoder errors into their own surface; renderer errors are renderer-internal.

---

## Frame upload cache *(future, post-MVP)*

A scrub burst over the same range repeatedly uploads the same frames. Cache keyed by frame identity (some stable per-frame hash or a `(SourceId, Rational pts)` if the renderer learns those from layers).

- Skip the CPU-side row-padding + `write_texture` if the cache hits.
- Eviction by bytes, similar to engine’s frame cache (4K YUV plane is ~6 MB).
- Decoder-engine layer might already cache decoded frames — this is a separate **GPU-side** cache.

**Optimization, not architecture.** MVP re-uploads every frame.

---

## Multi-layer composite *(future, post-MVP)*

Today: one layer, identity transform.

Tomorrow:

- N layers, drawn in order (back-to-front).
- Per-layer transform applied via per-draw uniform.
- Alpha blending for layers with `opacity < 1.0`.
- Target is a fixed canvas; layers position within it.

API doesn’t change — `render(&[Layer], &mut RenderTarget)` already takes a slice. Implementation grows to handle `len() > 1`.

---

## Filter graph / effects *(future, far-future)*

When effects exist:

- Each `Layer` carries a `Vec<Effect>` (or a graph reference).
- Renderer runs each layer’s effect chain into a per-layer intermediate texture, then composites.
- Effects are shader programs with their own bind groups + uniforms.
- Standard editor stuff: blur, color grade, LUT, crop, mask, etc.

Architecturally: a `Layer` becomes (frame + effect graph + transform), and the renderer becomes (per-layer effect pass → composite pass).

**Not a v1 concern.** Mentioned to keep the `Layer` shape extensible.

---

## Slint texture interop *(future, post-MVP)*

When the UI exists:

- Slint exposes a texture-sharing surface for native render output.
- Renderer’s `RenderTarget` becomes a Slint-aware texture (same `wgpu::Texture`, plus the bridge metadata Slint needs).
- UI thread doesn’t copy pixels — it references the GPU texture directly.

**Zero impact on the rest of the renderer.** Only `RenderTarget` construction changes.

---

## Multi-color-space support *(future, post-MVP)*

When this lands:

- Engine surfaces color metadata from decoder (`color_primaries`, `color_trc`, `color_space`, `color_range`).
- `DecodedVideoFrame` grows a `ColorInfo` field.
- Renderer’s shader picks matrix + range based on `ColorInfo`.
- Test fixtures grow: BT.601 SD source, full-range source, etc.

**Decoder needs work first** — surfacing color metadata is a small but real decoder extension.

---

## HDR *(future-future)*

PQ / HLG transfer functions, BT.2020 primaries, 10-bit input formats (`Yuv420p10le`, etc.), tone mapping for SDR display, scene-referred vs display-referred linear light, etc. This is its own multi-week mountain. Don’t even think about it until SDR is rock-solid.

---

## MVP scope (what we are doing today)

1. **wgpu offscreen setup** — `Device` + `Queue`, no `Surface`.
2. **Three pipelines** — `Yuv420p`, `Nv12`, `Rgba8`.
3. **Padded-row upload path** — handles wgpu’s `bytes_per_row` 256 alignment.
4. **BT.709 limited range YUV→RGB** — hardcoded, single matrix.
5. **`Layer` + `Transform`** types with identity defaults; `render(&[Layer], &mut RenderTarget)` API.
6. **Single-layer enforcement** (MVP asserts `len == 1`; multi-layer is non-breaking future).
7. **`RenderTarget`** owned by caller; renderer writes RGBA8 into it.
8. **Pixel readback** for tests and future export.
9. **`RendererError`** with the right granularity to debug failures.

## Out of scope today *(documented, deferred)*

- Texture upload cache (re-uploads every frame in MVP)
- Multi-layer composite
- Per-layer transforms and opacity *(types exist; renderer ignores them)*
- Filter graph / effects
- Color metadata pipeline (engine extension + renderer shader variants)
- BT.601, full range, BT.2020
- HDR / 10-bit
- Slint texture interop
- Surface-based rendering (windowed)
- Multi-renderer / multi-target draw batching

---

## Known limits (MVP)

Deliberate. Documented so future-us knows they were chosen.

- **Single layer per render call.** Calling with `len != 1` errors out.
- **BT.709 limited range hardcoded.** Off-spec input will look wrong.
- **No upload cache.** Re-uploads every frame; same scrub frame uploaded N times in a burst.
- **CPU row padding.** Extra memcpy per plane per frame; measure before claiming acceptable for 4K.
- **Offscreen only.** No window, no swapchain. Consumer composites into UI.
- **Single-threaded `Renderer`.** `Send` but not `Sync`; one per consumer thread.
- **No HDR, no 10-bit, no wide-gamut.** SDR rec.709 only.