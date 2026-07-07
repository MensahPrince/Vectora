// Full-canvas color grade pass. The input is the already-composited canvas;
// alpha passes through unchanged.

struct GradeUniforms {
    // exposure, brightness, contrast, saturation
    grade0: vec4<f32>,
    // temperature, tint, pad, pad
    grade1: vec4<f32>,
}

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var input_sampler: sampler;
@group(0) @binding(2) var<uniform> uniforms: GradeUniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(positions[vi], 0.0, 1.0);
    out.uv = uvs[vi];
    return out;
}

fn apply_grade(rgb: vec3<f32>, g0: vec4<f32>, g1: vec4<f32>) -> vec3<f32> {
    var c = rgb;
    c = c * exp2(2.0 * g0.x);
    c.r = c.r + 0.25 * g1.x;
    c.b = c.b - 0.25 * g1.x;
    c.g = c.g + 0.25 * g1.y;
    c = c + vec3<f32>(0.25 * g0.y);
    c = (c - vec3<f32>(0.5)) * (1.0 + g0.z) + vec3<f32>(0.5);
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = mix(vec3<f32>(luma), c, 1.0 + g0.w);
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let color = textureSample(input_tex, input_sampler, in.uv);
    return vec4<f32>(
        apply_grade(color.rgb, uniforms.grade0, uniforms.grade1),
        color.a,
    );
}
