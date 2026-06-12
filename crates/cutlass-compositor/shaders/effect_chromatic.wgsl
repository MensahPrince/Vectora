// effect_chromatic.wgsl — radial chromatic aberration (fragment only).
//
// p0.x = amount (0..1). Red samples pushed outward from the canvas centre and
// blue inward, so the channel fringing grows toward the corners like a cheap
// lens. Green stays put.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let amount = clamp(fx.p0.x, 0.0, 1.0) * 0.02;
    let dir = in.uv - vec2(0.5, 0.5);
    let cr = textureSample(src_tex, fx_sampler, in.uv + dir * amount);
    let cg = textureSample(src_tex, fx_sampler, in.uv);
    let cb = textureSample(src_tex, fx_sampler, in.uv - dir * amount);
    return vec4(cr.r, cg.g, cb.b, cg.a);
}
