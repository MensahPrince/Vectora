// transition_wipe_down.wgsl — a hard edge sweeps downward, revealing `to` from
// the top. At progress p the topmost p of the frame shows `to` (texture v
// grows downward, so the top is v <= p).

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = fx.p0.x;
    let c_from = textureSample(orig_tex, fx_sampler, in.uv);
    let c_to = textureSample(src_tex, fx_sampler, in.uv);
    if (in.uv.y <= p) {
        return c_to;
    }
    return c_from;
}
