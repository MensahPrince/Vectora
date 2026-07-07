// Separable box-blur approximation. `params.x` = radius in pixels;
// `params.y` = direction (0 = horizontal, 1 = vertical).

struct FullscreenUniforms {
    texel: vec4<f32>,
    params: vec4<f32>,
    progress: f32,
    wipe_dir: f32,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> u: FullscreenUniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

fn quad_corner(vertex_index: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    return corners[vertex_index];
}

@vertex
fn vs(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let c = quad_corner(vertex_index);
    var out: VertexOutput;
    out.position = vec4(c, 0.0, 1.0);
    out.uv = c * 0.5 + 0.5;
    out.uv.y = 1.0 - out.uv.y;
    return out;
}

@fragment
fn fs(in: VertexOutput) -> @location(0) vec4<f32> {
    let radius = max(u.params.x, 0.0);
    let horizontal = u.params.y < 0.5;
    let texel = select(vec2(u.texel.z, u.texel.w), vec2(u.texel.x, u.texel.y), horizontal);
    let samples = i32(clamp(radius, 1.0, 16.0));
    var acc = vec4(0.0);
    var weight_sum = 0.0;
    for (var i = -samples; i <= samples; i++) {
        let offset = vec2<f32>(f32(i), f32(i)) * texel * select(vec2(0.0, 1.0), vec2(1.0, 0.0), horizontal);
        let w = 1.0;
        acc += textureSample(input_tex, samp, in.uv + offset) * w;
        weight_sum += w;
    }
    return acc / weight_sum;
}
