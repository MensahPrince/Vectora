// transition_wipe_right.wgsl — a hard edge sweeps rightward, revealing `to`
// from the left. At progress p the leftmost p of the frame shows `to`.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = fx.p0.x;
    let c_from = textureSample(orig_tex, fx_sampler, in.uv);
    let c_to = textureSample(src_tex, fx_sampler, in.uv);
    if (in.uv.x <= p) {
        return c_to;
    }
    return c_from;
}
