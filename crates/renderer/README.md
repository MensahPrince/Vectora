# `renderer`

Offscreen [**wgpu**](https://wgpu.rs) rendering for Cutlass: **`DecodedVideoFrame`** (YUV420P, NV12, or RGBA8) → **RGBA8** in a caller-owned texture, with optional CPU readback for tests and export hooks.

## Quickstart

```rust
use renderer::{Layer, RenderTarget, Renderer, Transform};

let mut renderer = Renderer::new()?;
let target = RenderTarget::new(renderer.device(), width, height);
renderer.render(&[Layer { frame, transform: Transform::identity(), opacity: 1.0 }], &target)?;
let rgba = renderer.read_pixels_rgba8(&target)?;
```

MVP enforces **exactly one layer** per [`Renderer::render`](src/gpu.rs) call and expects the **render target size to match the frame** (identity transform / full-bleed).

## Examples

- `cargo run -p renderer --example wgpu_probe` — print the default adapter (e.g. Metal on macOS).
- `cargo run -p renderer --example end_to_end` — engine → decode → GPU → readback smoke (needs FFmpeg fixtures).

## Design docs

- Vision and shader notes: [`docs/renderer/research.md`](../docs/renderer/research.md)
- Phased implementation plan: [`docs/renderer/roadmap.md`](../docs/renderer/roadmap.md)

## Tests

```bash
cargo test -p renderer
cargo clippy -p renderer --all-targets -- -D warnings
```

Integration tests use video fixtures from `crates/decoder/tests/assets/` (see `regenerate.sh` there).
