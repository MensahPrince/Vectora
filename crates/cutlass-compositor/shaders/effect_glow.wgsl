// effect_glow.wgsl — bloom: extract bright areas, blur, add back (2 passes).
//
// p0.x = threshold (0..1, luma below which nothing glows), p0.y = intensity.
// Pass 0 (pass_info.x < 0.5): box-sample the placed layer, keep only the
// above-threshold brightness, and write the blurred bright map. Pass 1: read
// the untouched layer (orig_tex) and add intensity * the blurred bright map
// from the previous pass (src_tex).

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.299, 0.587, 0.114));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let threshold = clamp(fx.p0.x, 0.0, 1.0);
    let intensity = max(fx.p0.y, 0.0);

    if (fx.pass_info.x < 0.5) {
        let texel = fx.resolution.zw * 2.0;
        let headroom = max(1.0 - threshold, 0.001);
        var sum = vec3(0.0, 0.0, 0.0);
        for (var j = -2; j <= 2; j = j + 1) {
            for (var i = -2; i <= 2; i = i + 1) {
                let o = vec2(f32(i), f32(j)) * texel;
                let s = textureSample(src_tex, fx_sampler, in.uv + o);
                let bright = max(luma(s.rgb) - threshold, 0.0) / headroom;
                sum = sum + s.rgb * bright;
            }
        }
        return vec4(sum / 25.0, 1.0);
    }

    let orig = textureSample(orig_tex, fx_sampler, in.uv);
    let bloom = textureSample(src_tex, fx_sampler, in.uv).rgb;
    return vec4(orig.rgb + bloom * intensity, orig.a);
}
