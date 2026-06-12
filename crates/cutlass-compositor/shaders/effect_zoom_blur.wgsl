// effect_zoom_blur.wgsl — radial zoom blur about the canvas centre.
//
// p0.x = amount (0..1). Averages samples taken at slightly different zoom
// levels along the line from the centre through the pixel, smearing the frame
// radially. Premultiplied input, so a straight average is correct.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let amount = clamp(fx.p0.x, 0.0, 1.0);
    let center = vec2(0.5, 0.5);
    let radial = in.uv - center;
    var sum = vec4(0.0, 0.0, 0.0, 0.0);
    let n = 16;
    for (var i = 0; i < n; i = i + 1) {
        let t = f32(i) / f32(n - 1) - 0.5;
        let uv = center + radial * (1.0 + t * amount * 0.5);
        sum = sum + textureSample(src_tex, fx_sampler, uv);
    }
    return sum / f32(n);
}
