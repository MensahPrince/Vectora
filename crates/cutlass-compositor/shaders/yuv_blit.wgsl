// yuv_blit.wgsl — YUV420P → RGBA layer with optional upscale/downscale
//
// Upload Y/U/V as R8Unorm textures (U/V at half resolution). Maps canvas UV to
// source pixel coords with center-aligned bilinear sampling on Y and chroma.

struct YuvUniforms {
    src_size: vec2<f32>,
    dst_size: vec2<f32>,
}

@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var u_tex: texture_2d<f32>;
@group(0) @binding(2) var v_tex: texture_2d<f32>;
@group(0) @binding(3) var yuv_sampler: sampler;
@group(0) @binding(4) var<uniform> uniforms: YuvUniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vertex_index & 1u) * 4 - 1);
    let y = f32(i32(vertex_index >> 1u) * 4 - 1);
    out.position = vec4(x, y, 0.0, 1.0);
    out.uv = vec2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fn sample_plane(tex: texture_2d<f32>, samp: sampler, src_px: vec2<f32>, plane_size: vec2<f32>) -> f32 {
    let uv = (src_px + vec2(0.5)) / plane_size;
    let clamped = clamp(uv, vec2(0.0), vec2(1.0));
    return textureSample(tex, samp, clamped).r * 255.0;
}

fn yuv_to_rgb(yv: f32, uv: f32, vv: f32) -> vec3<f32> {
    let y = yv - 16.0;
    let u = uv - 128.0;
    let v = vv - 128.0;
    let r = clamp((298.0 * y + 409.0 * v + 128.0) / 256.0, 0.0, 255.0);
    let g = clamp((298.0 * y - 100.0 * u - 208.0 * v + 128.0) / 256.0, 0.0, 255.0);
    let b = clamp((298.0 * y + 516.0 * u + 128.0) / 256.0, 0.0, 255.0);
    return vec3(r, g, b) / 255.0;
}

@fragment
fn fs(in: VertexOutput) -> @location(0) vec4<f32> {
    let dst = uniforms.dst_size;
    let src = uniforms.src_size;
    let dst_px = vec2(in.uv.x * dst.x, in.uv.y * dst.y);
    let src_px = vec2(
        (dst_px.x + 0.5) * src.x / dst.x - 0.5,
        (dst_px.y + 0.5) * src.y / dst.y - 0.5,
    );
    let chroma_px = src_px * 0.5;
    let chroma_size = src * 0.5;

    let yv = sample_plane(y_tex, yuv_sampler, src_px, src);
    let uv = sample_plane(u_tex, yuv_sampler, chroma_px, chroma_size);
    let vv = sample_plane(v_tex, yuv_sampler, chroma_px, chroma_size);
    let rgb = yuv_to_rgb(yv, uv, vv);
    return vec4(rgb, 1.0);
}
