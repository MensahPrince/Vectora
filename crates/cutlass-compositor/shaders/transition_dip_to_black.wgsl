// transition_dip_to_black.wgsl — fade `from` down to black over the first
// half, then black up to `to`. Premultiplied: scaling rgb and a together keeps
// the colour valid; black is (0,0,0,0) premultiplied (fully transparent over
// the cleared canvas reads as black on an opaque background).

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = fx.p0.x;
    let c_from = textureSample(orig_tex, fx_sampler, in.uv);
    let c_to = textureSample(src_tex, fx_sampler, in.uv);
    if (p < 0.5) {
        return c_from * (1.0 - p * 2.0);
    }
    return c_to * (p * 2.0 - 1.0);
}
