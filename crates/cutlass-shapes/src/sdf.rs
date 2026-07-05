//! Signed-distance functions for the parametric shapes, plus a CPU
//! rasterizer built on them.
//!
//! **This module is the contract for `shape.wgsl` in `cutlass-compositor`.**
//! The shader implements these exact formulas (same shape math, same
//! [`coverage`] ramp, same [`SDF_AA`]); a golden test in the compositor
//! renders both and asserts per-pixel agreement. Change one side only in
//! lockstep with the other.
//!
//! Conventions shared by both implementations:
//!
//! - Coordinates are **output pixels**, origin at the shape center, +y down.
//!   The resolver folds clip scale into the pixel extents and rotation is
//!   rigid, so SDF distances are true canvas-pixel distances and a fixed
//!   anti-alias ramp is correct without derivative tricks.
//! - Distances are negative inside, positive outside.
//! - Stars and polygons share one evaluator: a regular polygon is the star
//!   whose inner vertices sit on its edge midpoints (see
//!   [`SdfShape::polygon`](crate::SdfShape::polygon)).

use cutlass_core::RgbaImage;

use crate::{SdfParams, SdfShape, ShapeStyle};

/// Anti-alias ramp half-width in pixels (see [`crate::AA`]).
pub const SDF_AA: f32 = 1.0;

/// Star spike count ceiling. Bounds the polygon evaluator's vertex loop on
/// both CPU and GPU (2 vertices per spike); model validation enforces the
/// same limit so a project can't carry a value the renderer would clamp.
pub const MAX_STAR_POINTS: u32 = 20;

/// Fill coverage of a pixel whose center sits at signed distance `d` from
/// the shape edge: a linear ramp from 1 (inside) to 0 (outside) across
/// `±SDF_AA`. Linear (not smoothstep) so the WGSL translation is trivially
/// identical.
pub fn coverage(d: f32) -> f32 {
    (0.5 - d / (2.0 * SDF_AA)).clamp(0.0, 1.0)
}

/// Signed distance (pixels) from point `p` (origin at shape center, +y
/// down) to `shape`'s edge. Negative inside.
pub fn eval(shape: &SdfShape, p: [f32; 2]) -> f32 {
    let half = shape.half;
    match shape.params {
        SdfParams::RoundedRect { radius } => {
            let r = radius.clamp(0.0, half[0].min(half[1]));
            sd_round_box(p, half, r)
        }
        SdfParams::Ellipse => sd_ellipse(p, half),
        SdfParams::Star {
            points,
            inner,
            round,
        } => {
            let round = round.clamp(0.0, half[0].min(half[1]));
            let mut verts = [[0.0f32; 2]; (2 * MAX_STAR_POINTS) as usize];
            let n = star_vertices(half, points, inner, round, &mut verts);
            sd_polygon(&verts[..n], p) - round
        }
        SdfParams::Line => sd_capsule(p, half),
        SdfParams::Arrow => {
            let verts = arrow_vertices(half);
            sd_polygon(&verts, p)
        }
        SdfParams::Heart => sd_heart(p, half),
    }
}

/// Rasterize `shape` with `style` to a straight-alpha bitmap, sized to the
/// shape plus stroke overhang plus the AA ramp. The shape center lands on
/// the image center.
///
/// This is the **CPU reference** for the GPU pipeline (used by golden tests
/// and available as a software fallback), not a per-frame path — it is a
/// plain O(w·h) loop.
pub fn raster(shape: &SdfShape, style: &ShapeStyle) -> RgbaImage {
    let half = shape.half;
    let stroke_w = style.stroke.map_or(0.0, |s| s.width.max(0.0));
    let pad = stroke_w * 0.5 + 2.0 * SDF_AA;
    let w = ((half[0] + pad) * 2.0).ceil().max(1.0) as u32;
    let h = ((half[1] + pad) * 2.0).ceil().max(1.0) as u32;

    let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    for y in 0..h {
        for x in 0..w {
            let p = [x as f32 + 0.5 - cx, y as f32 + 0.5 - cy];
            let d = eval(shape, p);
            let rgba = shade(d, style, stroke_w);
            if rgba[3] == 0 {
                continue;
            }
            let i = ((y * w + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&rgba);
        }
    }
    RgbaImage::new(w, h, pixels)
}

/// Resolve the color at signed distance `d`: stroke ring over fill, straight
/// alpha. Mirrors the WGSL fragment math exactly.
pub fn shade(d: f32, style: &ShapeStyle, stroke_w: f32) -> [u8; 4] {
    let fill_cov = match style.fill {
        Some(c) => coverage(d) * (c[3] as f32 / 255.0),
        None => 0.0,
    };
    let stroke_cov = match style.stroke {
        Some(s) if stroke_w > 0.0 => {
            coverage(d.abs() - stroke_w * 0.5) * (s.rgba[3] as f32 / 255.0)
        }
        _ => 0.0,
    };
    let fill = style.fill.unwrap_or([0; 4]);
    let stroke = style.stroke.map_or([0; 4], |s| s.rgba);

    // Straight-alpha "stroke over fill" for one pixel.
    let a = stroke_cov + fill_cov * (1.0 - stroke_cov);
    if a <= 0.0 {
        return [0; 4];
    }
    let mut out = [0u8; 4];
    for c in 0..3 {
        let s = stroke[c] as f32 / 255.0;
        let f = fill[c] as f32 / 255.0;
        let v = (s * stroke_cov + f * fill_cov * (1.0 - stroke_cov)) / a;
        out[c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out[3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
    out
}

// --- shape formulas (each has a WGSL twin in shape.wgsl) ---------------------

/// Exact rounded-box SDF (iq's `sdRoundBox`): box half-extents `b`, corner
/// radius `r` (already clamped by the caller).
fn sd_round_box(p: [f32; 2], b: [f32; 2], r: f32) -> f32 {
    let q = [p[0].abs() - b[0] + r, p[1].abs() - b[1] + r];
    let outside = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt();
    outside + q[0].max(q[1]).min(0.0) - r
}

/// Ellipse SDF via iq's quadratic approximation — exact on the axes and very
/// accurate near the edge (the only place AA samples it). The exact ellipse
/// distance needs a quartic solve; not worth it for a 1px ramp.
fn sd_ellipse(p: [f32; 2], r: [f32; 2]) -> f32 {
    let (rx, ry) = (r[0].max(1e-3), r[1].max(1e-3));
    let k1 = ((p[0] / rx).powi(2) + (p[1] / ry).powi(2)).sqrt();
    if k1 < 1e-6 {
        // Dead center: distance to the nearest axis end.
        return -rx.min(ry);
    }
    let k2 = ((p[0] / (rx * rx)).powi(2) + (p[1] / (ry * ry)).powi(2)).sqrt();
    k1 * (k1 - 1.0) / k2
}

/// Horizontal capsule: total length `2*half[0]`, thickness `2*half[1]`,
/// round caps (caps eat into the length so the shape stays inside the box).
fn sd_capsule(p: [f32; 2], half: [f32; 2]) -> f32 {
    let r = half[1].min(half[0]);
    let straight = (half[0] - r).max(0.0);
    let qx = (p[0].abs() - straight).max(0.0);
    (qx * qx + p[1] * p[1]).sqrt() - r
}

/// Exact polygon SDF (iq's `sdPolygon`): minimum distance to the edge loop,
/// sign from even-odd crossing parity. O(vertices) — bounded by
/// [`MAX_STAR_POINTS`] on the star path and 7 on the arrow path.
fn sd_polygon(v: &[[f32; 2]], p: [f32; 2]) -> f32 {
    let n = v.len();
    let mut d = dot2(sub(p, v[0]));
    let mut s = 1.0f32;
    let mut j = n - 1;
    for i in 0..n {
        let e = sub(v[j], v[i]);
        let w = sub(p, v[i]);
        let t = (dot(w, e) / dot2(e).max(1e-12)).clamp(0.0, 1.0);
        let b = [w[0] - e[0] * t, w[1] - e[1] * t];
        d = d.min(dot2(b));
        let c0 = p[1] >= v[i][1];
        let c1 = p[1] < v[j][1];
        let c2 = e[0] * w[1] > e[1] * w[0];
        if (c0 && c1 && c2) || (!c0 && !c1 && !c2) {
            s = -s;
        }
        j = i;
    }
    s * d.sqrt()
}

/// The `2*points` vertices of a star, spike up, inscribed in the box's
/// ellipse: outer vertices at the (aspect-scaled) unit circle, inner at
/// `inner` of it. `round` shrinks the vertex radii so the rounded shape
/// (`sd - round`) stays inside the box. Returns the vertex count.
fn star_vertices(
    half: [f32; 2],
    points: u32,
    inner: f32,
    round: f32,
    out: &mut [[f32; 2]; (2 * MAX_STAR_POINTS) as usize],
) -> usize {
    let points = points.clamp(3, MAX_STAR_POINTS);
    let inner = inner.clamp(0.05, 1.0);
    let (hx, hy) = ((half[0] - round).max(0.5), (half[1] - round).max(0.5));
    let n = (2 * points) as usize;
    let step = std::f32::consts::PI / points as f32;
    for (k, v) in out.iter_mut().take(n).enumerate() {
        // Start at -pi/2 so vertex 0 points up (+y is down in canvas space).
        let theta = -std::f32::consts::FRAC_PI_2 + step * k as f32;
        let r = if k % 2 == 0 { 1.0 } else { inner };
        *v = [theta.cos() * r * hx, theta.sin() * r * hy];
    }
    n
}

/// Right-pointing arrow as an explicit 7-gon: head length is the box
/// half-height (clamped by half the width), shaft is 40% of the box height.
/// Fixed drop proportions; width/height params stretch the whole figure.
fn arrow_vertices(half: [f32; 2]) -> [[f32; 2]; 7] {
    let (hx, hy) = (half[0], half[1]);
    let head = hy.min(hx);
    let shaft = 0.4 * hy;
    [
        [hx, 0.0],           // tip
        [hx - head, -hy],    // head top
        [hx - head, -shaft], // notch top
        [-hx, -shaft],       // tail top
        [-hx, shaft],        // tail bottom
        [hx - head, shaft],  // notch bottom
        [hx - head, hy],     // head bottom
    ]
}

/// Heart SDF (iq's `sdHeart`), mapped so the unit heart fills the box
/// upright. The unit heart spans `x ∈ ±(1/4 + √2/4)`, `y ∈ 0..=(3/4 + √2/4)`
/// (tip at the origin, lobes up); we scale each axis to the box and convert
/// the unit distance back to pixels with the smaller axis scale (exact for
/// uniform boxes, a safe underestimate otherwise).
fn sd_heart(p: [f32; 2], half: [f32; 2]) -> f32 {
    let lobe = std::f32::consts::SQRT_2 / 4.0;
    let unit_hw = 0.25 + lobe; // half-width of the unit heart
    let unit_h = 0.75 + lobe; // full height of the unit heart

    let sx = half[0] / unit_hw;
    let sy = half[1] / (unit_h * 0.5);
    // Heart space: x mirrored around 0, y up from the tip.
    let hx = (p[0] / sx).abs();
    let hy = (half[1] - p[1]) / sy;

    let d_unit = if hy + hx > 1.0 {
        (dot2([hx - 0.25, hy - 0.75])).sqrt() - lobe
    } else {
        let a = dot2([hx, hy - 1.0]);
        let m = 0.5 * (hx + hy).max(0.0);
        let b = dot2([hx - m, hy - m]);
        a.min(b).sqrt() * (hx - hy).signum()
    };
    d_unit * sx.min(sy)
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn dot(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

fn dot2(a: [f32; 2]) -> f32 {
    dot(a, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SdfParams, Stroke};

    fn fill(rgba: [u8; 4]) -> ShapeStyle {
        ShapeStyle {
            fill: Some(rgba),
            stroke: None,
        }
    }

    #[test]
    fn rounded_rect_signs_and_corner() {
        let s = SdfParams::RoundedRect { radius: 10.0 }.with_half([50.0, 30.0]);
        assert!(eval(&s, [0.0, 0.0]) < 0.0, "center is inside");
        assert!(eval(&s, [60.0, 0.0]) > 0.0, "beyond +x is outside");
        // Sharp-corner point is shaved off by the rounding.
        assert!(eval(&s, [49.5, 29.5]) > 0.0);
        // Edge midpoints sit on the boundary.
        assert!(eval(&s, [50.0, 0.0]).abs() < 1e-3);
        assert!(eval(&s, [0.0, 30.0]).abs() < 1e-3);
    }

    #[test]
    fn ellipse_axis_points_are_exact() {
        let s = SdfParams::Ellipse.with_half([40.0, 20.0]);
        assert!(eval(&s, [40.0, 0.0]).abs() < 1e-3);
        assert!(eval(&s, [0.0, 20.0]).abs() < 1e-3);
        assert!(eval(&s, [0.0, 0.0]) <= -19.0, "center is deep inside");
        assert!(eval(&s, [40.0, 20.0]) > 0.0, "box corner is outside");
    }

    #[test]
    fn polygon_is_star_with_midpoint_inners() {
        // A triangle: apex up, base down, inscribed in the circle.
        let tri = SdfParams::polygon(3, 0.0).with_half([50.0, 50.0]);
        assert!(eval(&tri, [0.0, -49.0]) < 0.0, "apex region inside");
        assert!(eval(&tri, [0.0, 49.0]) > 0.0, "below base is outside");
        assert!(eval(&tri, [0.0, 0.0]) < 0.0, "centroid inside");
        // The base edge of an inscribed up-triangle sits at y = +h/2 * cos60·2…
        // just check left/right symmetry instead of chasing constants.
        let l = eval(&tri, [-20.0, 10.0]);
        let r = eval(&tri, [20.0, 10.0]);
        assert!((l - r).abs() < 1e-4, "triangle SDF must be x-symmetric");
    }

    #[test]
    fn star_spikes_alternate_inside_outside() {
        let s = SdfParams::Star {
            points: 5,
            inner: 0.5,
            round: 0.0,
        }
        .with_half([50.0, 50.0]);
        // Top spike tip is on the boundary; between spikes (rotated by one
        // half-step at the same radius) is far outside.
        assert!(eval(&s, [0.0, -50.0]).abs() < 0.5);
        let between = std::f32::consts::PI / 5.0;
        let p = [
            (-std::f32::consts::FRAC_PI_2 + between).cos() * 50.0,
            (-std::f32::consts::FRAC_PI_2 + between).sin() * 50.0,
        ];
        assert!(eval(&s, p) > 5.0);
        assert!(eval(&s, [0.0, 0.0]) < 0.0);
    }

    #[test]
    fn capsule_ends_are_round() {
        let s = SdfParams::Line.with_half([60.0, 6.0]);
        assert!(eval(&s, [0.0, 0.0]) < 0.0);
        assert!(eval(&s, [60.0, 0.0]).abs() < 1e-3, "tip on boundary");
        // The box corner is outside (round cap, not square): exactly
        // r·(√2−1) ≈ 2.49px beyond a radius-6 cap.
        assert!(eval(&s, [60.0, 6.0]) > 2.0);
    }

    #[test]
    fn arrow_tip_shaft_and_notch() {
        let s = SdfParams::Arrow.with_half([60.0, 25.0]);
        assert!(eval(&s, [59.0, 0.0]) < 0.0, "just inside the tip");
        assert!(eval(&s, [-59.0, 0.0]) < 0.0, "inside the tail");
        // Above the shaft, behind the head: outside (the notch).
        assert!(eval(&s, [0.0, -20.0]) > 0.0);
        assert!(eval(&s, [0.0, -5.0]) < 0.0, "inside the shaft");
    }

    #[test]
    fn heart_lobes_and_tip() {
        let s = SdfParams::Heart.with_half([50.0, 50.0]);
        assert!(eval(&s, [0.0, 49.5]).abs() < 1.0, "tip at box bottom");
        assert!(eval(&s, [-25.0, -25.0]) < 0.0, "left lobe inside");
        assert!(eval(&s, [25.0, -25.0]) < 0.0, "right lobe inside");
        assert!(eval(&s, [0.0, -49.0]) > 0.0, "cleft between lobes");
        let l = eval(&s, [-30.0, 10.0]);
        let r = eval(&s, [30.0, 10.0]);
        assert!((l - r).abs() < 1e-4, "heart SDF must be x-symmetric");
    }

    #[test]
    fn raster_fill_center_and_transparent_corner() {
        let img = raster(
            &SdfParams::Ellipse.with_half([20.0, 12.0]),
            &fill([255, 0, 0, 255]),
        );
        assert!(img.is_well_formed());
        let c = img.pixel(img.width / 2, img.height / 2);
        assert_eq!(c, [255, 0, 0, 255]);
        assert_eq!(img.pixel(0, 0)[3], 0, "padded corner is transparent");
    }

    #[test]
    fn raster_stroke_rings_the_fill() {
        let style = ShapeStyle {
            fill: Some([0, 0, 255, 255]),
            stroke: Some(Stroke {
                rgba: [255, 255, 0, 255],
                width: 6.0,
            }),
        };
        let shape = SdfParams::RoundedRect { radius: 0.0 }.with_half([30.0, 20.0]);
        let img = raster(&shape, &style);
        let cx = img.width / 2;
        let cy = img.height / 2;
        // Center: pure fill.
        assert_eq!(img.pixel(cx, cy), [0, 0, 255, 255]);
        // On the right edge (x = +30 from center): pure stroke color.
        let edge = img.pixel(cx + 30, cy);
        assert_eq!(
            [edge[0], edge[1], edge[2]],
            [255, 255, 0],
            "edge pixel should be stroke-colored, got {edge:?}"
        );
        // Well outside the stroke: transparent.
        assert_eq!(img.pixel(cx + 40, cy)[3], 0);
    }

    #[test]
    fn stroke_alpha_folds_into_coverage() {
        let translucent = ShapeStyle {
            fill: None,
            stroke: Some(Stroke {
                rgba: [255, 255, 255, 128],
                width: 4.0,
            }),
        };
        let img = raster(&SdfParams::Ellipse.with_half([20.0, 20.0]), &translucent);
        let cx = img.width / 2;
        let cy = img.height / 2;
        let on_edge = img.pixel(cx + 20, cy);
        assert!(
            (on_edge[3] as i32 - 128).abs() <= 4,
            "stroke alpha should cap coverage, got {}",
            on_edge[3]
        );
        assert_eq!(
            img.pixel(cx, cy)[3],
            0,
            "no fill inside a stroke-only shape"
        );
    }

    #[test]
    fn hit_test_includes_stroke_overhang() {
        let s = SdfParams::Ellipse.with_half([20.0, 20.0]);
        assert!(s.hit_test([0.0, 0.0], 0.0));
        assert!(!s.hit_test([23.0, 0.0], 0.0));
        // A 10px stroke extends 5px beyond the edge.
        assert!(s.hit_test([23.0, 0.0], 10.0));
    }

    #[test]
    fn coverage_ramp_endpoints() {
        assert_eq!(coverage(-SDF_AA), 1.0);
        assert_eq!(coverage(SDF_AA), 0.0);
        assert!((coverage(0.0) - 0.5).abs() < 1e-6);
    }
}
