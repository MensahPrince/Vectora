// transition_wipe_left.wgsl — a hard edge sweeps leftward, revealing `to`
// from the right. At progress p the rightmost p of the frame shows `to`.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = fx.p0.x;
    let c_from = textureSample(orig_tex, fx_sampler, in.uv);
    let c_to = textureSample(src_tex, fx_sampler, in.uv);
    if (in.uv.x >= 1.0 - p) {
        return c_to;
    }
    return c_from;
}
