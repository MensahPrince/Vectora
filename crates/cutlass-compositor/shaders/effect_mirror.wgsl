// effect_mirror.wgsl — kaleidoscope-style fold across an axis.
//
// p0.x = mode: 0 left→right, 1 right→left, 2 top→bottom, 3 bottom→top. The
// chosen half of the frame is reflected onto the other half.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let mode = i32(fx.p0.x + 0.5);
    var uv = in.uv;
    if (mode == 0) {
        uv.x = select(uv.x, 1.0 - uv.x, uv.x > 0.5);
    } else if (mode == 1) {
        uv.x = select(uv.x, 1.0 - uv.x, uv.x < 0.5);
    } else if (mode == 2) {
        uv.y = select(uv.y, 1.0 - uv.y, uv.y > 0.5);
    } else {
        uv.y = select(uv.y, 1.0 - uv.y, uv.y < 0.5);
    }
    return textureSample(src_tex, fx_sampler, uv);
}
