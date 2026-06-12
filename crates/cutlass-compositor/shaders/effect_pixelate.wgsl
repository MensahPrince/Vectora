// effect_pixelate.wgsl — mosaic / blockify (fragment only).
//
// p0.x = cell size in pixels. Each output pixel reads the centre of the cell
// it falls in, so the frame collapses to fixed-size blocks.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let cell = max(fx.p0.x, 1.0);
    let px = in.uv * fx.resolution.xy;
    let snapped = (floor(px / cell) + vec2(0.5, 0.5)) * cell;
    return textureSample(src_tex, fx_sampler, snapped * fx.resolution.zw);
}
