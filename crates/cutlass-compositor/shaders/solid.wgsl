// solid.wgsl — placed solid fill layer
//
// Used by LayerContent::Solid (e.g. Generator::SolidColor clips). Draws a
// single RGBA color across the layer's placed quad (full canvas at identity
// transform; a positioned/rotated rect otherwise).
//
// Pipeline: compositor.rs `solid_pipeline`
//   - Render target: Rgba8Unorm offscreen texture
//   - Load: Clear transparent on first layer, then Load for subsequent layers
//   - Blend (configured in Rust, not here): src-over
//       color:  SrcAlpha * src + (1 - SrcAlpha) * dst
//       alpha:  1 * src.a + (1 - SrcAlpha) * dst.a
//
// Input color is straight (non-premultiplied) RGBA in 0..1, uploaded from
// engine as u8 0–255 and normalized in compositor.rs. Layer opacity
// multiplies the fill alpha.

struct Uniforms {
    color: vec4<f32>,
    // Columns of the 2x2 linear part mapping unit-quad corners to clip
    // space: (m00, m10, m01, m11).
    linear: vec4<f32>,
    // Clip-space translation (x, y), layer opacity, pad.
    trans_opacity: vec4<f32>,
    // Content UV rect — unused for solid fills, present so the uniform
    // layout matches `PlacementUniforms` embedded in Rust's SolidUniforms.
    uv_rect: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
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
    let m = uniforms.linear;
    let t = uniforms.trans_opacity;
    var out: VertexOutput;
    out.position = vec4(
        m.x * c.x + m.z * c.y + t.x,
        m.y * c.x + m.w * c.y + t.y,
        0.0,
        1.0,
    );
    return out;
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4(uniforms.color.rgb, uniforms.color.a * uniforms.trans_opacity.z);
}
