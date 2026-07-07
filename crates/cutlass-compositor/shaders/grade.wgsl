// grade.wgsl — per-layer color grade in the fragment shader (no extra pass).
//
// Each pipeline appends `grade_adj0` / `grade_adj1` to its uniform block:
//   adj0 = brightness, contrast, saturation, enabled (0 | 1)
//   adj1 = exposure, temperature, tint, pad
//
// Params are signed strengths in roughly [-1, 1]; `enabled == 0` is the
// identity fast path (one branch, no math).

fn apply_color_grade(rgb: vec3<f32>, adj0: vec4<f32>, adj1: vec4<f32>) -> vec3<f32> {
    if (adj0.w < 0.5) {
        return rgb;
    }
    var c = rgb;
    c *= exp2(2.0 * adj1.x);
    c.r += adj1.y * 0.25;
    c.b -= adj1.y * 0.25;
    c.g += adj1.z * 0.25;
    c += adj0.x * 0.25;
    c = (c - vec3(0.5)) * (1.0 + adj0.y) + vec3(0.5);
    let luma = dot(c, vec3(0.2126, 0.7152, 0.0722));
    c = mix(vec3(luma), c, 1.0 + adj0.z);
    return clamp(c, vec3(0.0), vec3(1.0));
}
