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

@group(0) @binding(0) var t_rgba: texture_2d<f32>;
@group(0) @binding(1) var s_linear: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_rgba, s_linear, in.uv);
}
