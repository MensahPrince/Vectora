// effect_grain.wgsl — additive film grain (fragment only).
//
// p0.x = amount (0..1), p0.y = seed. Per-pixel value noise added to rgb.
// Premultiplied input: the noise is scaled by coverage (alpha) so transparent
// regions stay transparent, and rgb is clamped to [0, alpha].

fn hash2(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let amount = clamp(fx.p0.x, 0.0, 1.0);
    let seed = fx.p0.y;
    let c = textureSample(src_tex, fx_sampler, in.uv);
    let n = (hash2(in.uv * fx.resolution.xy + vec2(seed, seed * 2.0)) - 0.5) * amount;
    let rgb = clamp(c.rgb + n * c.a, vec3(0.0), vec3(c.a));
    return vec4(rgb, c.a);
}
