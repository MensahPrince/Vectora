// grade.wgsl — per-layer color grade on gamma-encoded RGB in [0, 1].
//
// g0 = (exposure, brightness, contrast, saturation); g1 = (temperature, tint, pad, pad).
// All controls are neutral at 0; applied in a fixed order then clamped.

fn apply_grade(rgb: vec3<f32>, g0: vec4<f32>, g1: vec4<f32>) -> vec3<f32> {
    var c = rgb;
    c = c * exp2(2.0 * g0.x);
    c.r = c.r + 0.25 * g1.x;
    c.b = c.b - 0.25 * g1.x;
    c.g = c.g + 0.25 * g1.y;
    c = c + vec3(0.25 * g0.y);
    c = (c - vec3(0.5)) * (1.0 + g0.z) + vec3(0.5);
    let luma = dot(c, vec3(0.2126, 0.7152, 0.0722));
    c = mix(vec3(luma), c, 1.0 + g0.w);
    return clamp(c, vec3(0.0), vec3(1.0));
}
