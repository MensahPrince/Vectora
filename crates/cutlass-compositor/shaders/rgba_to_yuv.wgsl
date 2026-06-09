// rgba_to_yuv.wgsl — RGBA8Unorm → packed YUV420P (BT.601) via compute

struct RgbaToYuvParams {
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
}

@group(0) @binding(0) var rgba_tex: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> y_out: array<u32>;
@group(0) @binding(2) var<storage, read_write> u_out: array<u32>;
@group(0) @binding(3) var<storage, read_write> v_out: array<u32>;
@group(0) @binding(4) var<uniform> params: RgbaToYuvParams;

fn rgb_to_yuv(r: f32, g: f32, b: f32) -> vec3<f32> {
    // Match legacy CPU coeffs in cutlass-compositor::legacy_rgba_to_yuv420p.
    let y = clamp((66.0 * r + 129.0 * g + 25.0 * b + 128.0) / 256.0, 16.0, 235.0);
    let u = clamp((-38.0 * r - 74.0 * g + 112.0 * b + 128.0) / 256.0, 16.0, 240.0);
    let v = clamp((112.0 * r - 94.0 * g - 18.0 * b + 128.0) / 256.0, 16.0, 240.0);
    return vec3(y, u, v);
}

@compute @workgroup_size(8, 8)
fn cs(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= params.width || y >= params.height) {
        return;
    }

    let rgba = textureLoad(rgba_tex, vec2<i32>(i32(x), i32(y)), 0);
    let yuv = rgb_to_yuv(rgba.r * 255.0, rgba.g * 255.0, rgba.b * 255.0);
    let y_idx = y * params.y_stride + x;
    y_out[y_idx] = u32(yuv.x);

    if ((x & 1u) == 0u && (y & 1u) == 0u) {
        var r_sum = rgba.r * 255.0;
        var g_sum = rgba.g * 255.0;
        var b_sum = rgba.b * 255.0;
        var count = 1.0;

        if (x + 1u < params.width) {
            let p1 = textureLoad(rgba_tex, vec2<i32>(i32(x + 1u), i32(y)), 0);
            r_sum += p1.r * 255.0;
            g_sum += p1.g * 255.0;
            b_sum += p1.b * 255.0;
            count += 1.0;
        }
        if (y + 1u < params.height) {
            let p2 = textureLoad(rgba_tex, vec2<i32>(i32(x), i32(y + 1u)), 0);
            r_sum += p2.r * 255.0;
            g_sum += p2.g * 255.0;
            b_sum += p2.b * 255.0;
            count += 1.0;
        }
        if (x + 1u < params.width && y + 1u < params.height) {
            let p3 = textureLoad(rgba_tex, vec2<i32>(i32(x + 1u), i32(y + 1u)), 0);
            r_sum += p3.r * 255.0;
            g_sum += p3.g * 255.0;
            b_sum += p3.b * 255.0;
            count += 1.0;
        }

        let avg = rgb_to_yuv(r_sum / count, g_sum / count, b_sum / count);
        let uv_x = x / 2u;
        let uv_y = y / 2u;
        let u_idx = uv_y * params.uv_stride + uv_x;
        let v_idx = uv_y * params.uv_stride + uv_x;
        u_out[u_idx] = u32(avg.y);
        v_out[v_idx] = u32(avg.z);
    }
}
