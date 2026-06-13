// transition_dip_to_white.wgsl — fade `from` up to white over the first half,
// then white down to `to`. White is opaque (1,1,1,1) in premultiplied space.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = fx.p0.x;
    let white = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    let c_from = textureSample(orig_tex, fx_sampler, in.uv);
    let c_to = textureSample(src_tex, fx_sampler, in.uv);
    if (p < 0.5) {
        return mix(c_from, white, p * 2.0);
    }
    return mix(white, c_to, p * 2.0 - 1.0);
}
