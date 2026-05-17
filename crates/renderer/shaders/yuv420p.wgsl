struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    let x = f32((idx << 1u) & 2u);
    let y = f32(idx & 2u);
    var out: VertexOutput;
    out.uv = vec2(x, 1.0 - y);
    out.pos = vec4(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

@group(0) @binding(0) var t_y: texture_2d<f32>;
@group(0) @binding(1) var t_u: texture_2d<f32>;
@group(0) @binding(2) var t_v: texture_2d<f32>;
@group(0) @binding(3) var s_linear: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let y = textureSample(t_y, s_linear, in.uv).r;
    let u = textureSample(t_u, s_linear, in.uv).r;
    let v = textureSample(t_v, s_linear, in.uv).r;

    // BT.709 limited range (8-bit studio swing)
    let yp = (y - 16.0 / 255.0) * 255.0 / 219.0;
    let up = (u - 128.0 / 255.0) * 255.0 / 224.0;
    let vp = (v - 128.0 / 255.0) * 255.0 / 224.0;

    let r = yp + 1.5748 * vp;
    let g = yp - 0.1873 * up - 0.4681 * vp;
    let b = yp + 1.8556 * up;

    return vec4(clamp(vec3(r, g, b), vec3(0.0), vec3(1.0)), 1.0);
}
