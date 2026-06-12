// blit.wgsl — placed textured quad (RGBA upload)
//
// Used by LayerContent::Rgba: a CPU-decoded RGBA8 buffer or generator raster
// uploaded as a GPU texture each frame and drawn as a positioned, rotated,
// scaled quad (LayerPlacement). Linear filtering covers scaled layers; at 1:1
// full-canvas placement sampling lands exactly on texel centers (bit-exact).
//
// Pipeline: compositor.rs `blit_pipeline`
//   - Same render target and src-over blend as solid.wgsl
//   - Layer textures are Rgba8Unorm, COPY_DST + TEXTURE_BINDING
//
// Geometry: a unit quad ([-0.5, 0.5]², +y down in content space) mapped to
// clip space by the per-layer affine in `placement` (computed in Rust:
// canvas placement composed with canvas→clip). Corner (-0.5, -0.5) is the
// content's top-left ⇒ UV (0, 0), matching row-major top-first RGBA.

struct Placement {
    // Columns of the 2x2 linear part mapping unit-quad corners to clip
    // space: (m00, m10, m01, m11).
    linear: vec4<f32>,
    // Clip-space translation (x, y), layer opacity, pad.
    trans_opacity: vec4<f32>,
    // Content UV rect (u0, v0, u1, v1) interpolated across the quad:
    // sub-rects crop, reversed axes mirror (flip H/V).
    uv_rect: vec4<f32>,
}

@group(0) @binding(0) var layer_tex: texture_2d<f32>;
@group(0) @binding(1) var layer_sampler: sampler;
@group(0) @binding(2) var<uniform> placement: Placement;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

fn quad_corner(vertex_index: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2(-0.5, -0.5), vec2(0.5, -0.5), vec2(-0.5, 0.5),
        vec2(-0.5, 0.5), vec2(0.5, -0.5), vec2(0.5, 0.5),
    );
    return corners[vertex_index];
}

@vertex
fn vs(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let c = quad_corner(vertex_index);
    let m = placement.linear;
    let t = placement.trans_opacity;
    var out: VertexOutput;
    out.position = vec4(
        m.x * c.x + m.z * c.y + t.x,
        m.y * c.x + m.w * c.y + t.y,
        0.0,
        1.0,
    );
    out.uv = mix(placement.uv_rect.xy, placement.uv_rect.zw, c + vec2(0.5, 0.5));
    return out;
}

@fragment
fn fs(in: VertexOutput) -> @location(0) vec4<f32> {
    let px = textureSample(layer_tex, layer_sampler, in.uv);
    return vec4(px.rgb, px.a * placement.trans_opacity.z);
}
