// transition_wipe_up.wgsl — a hard edge sweeps upward, revealing `to` from the
// bottom. At progress p the lowest p of the frame shows `to` (texture v grows
// downward, so the bottom is v >= 1 - p).

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = fx.p0.x;
    let c_from = textureSample(orig_tex, fx_sampler, in.uv);
    let c_to = textureSample(src_tex, fx_sampler, in.uv);
    if (in.uv.y >= 1.0 - p) {
        return c_to;
    }
    return c_from;
}
