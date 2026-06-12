// effect_sharpen.wgsl — unsharp mask (fragment only).
//
// p0.x = amount (0 = off). Center weight (1 + 4a) minus the 4-neighbour cross
// times a. Premultiplied input, so rgb is clamped to [0, alpha].

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let amount = max(fx.p0.x, 0.0);
    let dx = vec2(fx.resolution.z, 0.0);
    let dy = vec2(0.0, fx.resolution.w);
    let c = textureSample(src_tex, fx_sampler, in.uv);
    let n = textureSample(src_tex, fx_sampler, in.uv + dy)
          + textureSample(src_tex, fx_sampler, in.uv - dy)
          + textureSample(src_tex, fx_sampler, in.uv + dx)
          + textureSample(src_tex, fx_sampler, in.uv - dx);
    let sharp = c * (1.0 + 4.0 * amount) - n * amount;
    return vec4(clamp(sharp.rgb, vec3(0.0), vec3(c.a)), c.a);
}
