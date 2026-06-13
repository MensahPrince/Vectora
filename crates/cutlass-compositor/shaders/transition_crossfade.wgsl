// transition_crossfade.wgsl — linear dissolve from `from` (orig_tex) to `to`
// (src_tex). Scratch holds premultiplied alpha, so a straight mix is correct.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = fx.p0.x;
    let c_from = textureSample(orig_tex, fx_sampler, in.uv);
    let c_to = textureSample(src_tex, fx_sampler, in.uv);
    return mix(c_from, c_to, p);
}
