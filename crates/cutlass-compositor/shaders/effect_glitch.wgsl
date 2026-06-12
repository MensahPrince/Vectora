// effect_glitch.wgsl — banded RGB-split + row displacement (fragment only).
//
// p0.x = amount (0..1), p0.y = seed (shifts which rows tear). A handful of
// horizontal bands jump sideways and the colour channels separate, the
// classic digital-tear look. Deterministic for a given seed.

fn hash(p: f32) -> f32 {
    return fract(sin(p * 12.9898) * 43758.5453);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let amount = clamp(fx.p0.x, 0.0, 1.0);
    let seed = fx.p0.y;
    // Quantise to 24 horizontal bands; only the brightest-hashed bands tear.
    let band = floor(in.uv.y * 24.0);
    let r = hash(band + seed * 57.0);
    let jump = step(0.7, r) * (hash(band * 1.7 + seed) - 0.5) * amount * 0.2;
    let uv = vec2(in.uv.x + jump, in.uv.y);
    let off = amount * 0.012;
    let cr = textureSample(src_tex, fx_sampler, uv + vec2(off, 0.0));
    let cg = textureSample(src_tex, fx_sampler, uv);
    let cb = textureSample(src_tex, fx_sampler, uv - vec2(off, 0.0));
    return vec4(cr.r, cg.g, cb.b, cg.a);
}
