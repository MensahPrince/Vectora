// transition_slide.wgsl — `to` pushes in from the right while `from` slides
// out to the left (a push). At progress p the boundary sits at x = 1 - p:
// left of it shows `from` shifted left by p, right of it shows `to` shifted in
// from the right.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let p = fx.p0.x;
    if (in.uv.x < 1.0 - p) {
        let uv = vec2<f32>(in.uv.x + p, in.uv.y);
        return textureSample(orig_tex, fx_sampler, uv);
    }
    let uv = vec2<f32>(in.uv.x - (1.0 - p), in.uv.y);
    return textureSample(src_tex, fx_sampler, uv);
}
