//! Preview viewport zoom/pan math (the inspect transform, not the project).
//!
//! The docked preview can be zoomed and panned like a canvas — purely a way to
//! inspect the composited frame, never a project/clip edit. Slint owns the
//! gesture plumbing (Ctrl/Cmd-scroll & trackpad pinch to zoom, two-finger
//! scroll / drag to pan); the actual geometry — how far you can pan, how a
//! cursor-anchored zoom moves the pan, the zoom clamp — lives here so it's
//! pure and unit-tested. The resulting `(zoom, pan)` flows back into
//! [`crate::preview_select`]'s viewport mapping, so hit-testing, the selection
//! box, and transform gestures all stay pixel-aligned at any zoom.
//!
//! Pan is in viewport logical px, measuring the content center's offset from
//! the viewport center (so `pan = 0` is centered). Zoom is a multiple of the
//! aspect-fit ("fit") scale: `1.0` fills the viewport like `ImageFit.contain`,
//! `< 1` shrinks the frame inside it, `> 1` magnifies past the edges.

use crate::PreviewView;
use crate::preview_select::contain_mapping;

/// Smallest inspect zoom — well below fit so the frame can be scaled down to a
/// thumbnail (CapCut lets you pull the canvas way out).
pub const MIN_ZOOM: f32 = 0.1;
/// Largest inspect zoom (8× the fitted size).
pub const MAX_ZOOM: f32 = 8.0;
/// Aspect-fit zoom: the frame exactly fills the viewport (`ImageFit.contain`).
pub const FIT_ZOOM: f32 = 1.0;

fn clamp_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        FIT_ZOOM
    }
}

/// The fitted content size in viewport px at `zoom == 1` — the aspect-fit of
/// the canvas into the viewport (the same `contain_mapping` the renderer and
/// hit-test use), so pan limits match the pixels exactly.
fn fitted_dims(canvas_w: f32, canvas_h: f32, view_w: f32, view_h: f32) -> (f32, f32) {
    let (scale, _, _) = contain_mapping(canvas_w, canvas_h, view_w, view_h);
    (canvas_w * scale, canvas_h * scale)
}

/// Clamp one pan axis so the content can't be dragged past the point where its
/// far edge meets the viewport edge. When the content is smaller than the
/// viewport on this axis (zoomed out, or the letterboxed axis), the range
/// collapses to 0 — it stays centered, no dead space to scroll into.
fn clamp_pan_axis(content: f32, view: f32, pan: f32) -> f32 {
    let max = ((content - view) / 2.0).max(0.0);
    pan.clamp(-max, max)
}

/// Normalize a viewport state: clamp the zoom into range, then clamp the pan to
/// what that zoom actually allows. Every public entry point funnels through
/// here, so the returned state is always renderable.
pub fn clamp_view(
    canvas_w: f32,
    canvas_h: f32,
    view_w: f32,
    view_h: f32,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
) -> PreviewView {
    let zoom = clamp_zoom(zoom);
    let (fw, fh) = fitted_dims(canvas_w, canvas_h, view_w, view_h);
    PreviewView {
        zoom,
        pan_x: clamp_pan_axis(fw * zoom, view_w, pan_x),
        pan_y: clamp_pan_axis(fh * zoom, view_h, pan_y),
    }
}

/// Zoom toward `target_zoom` while keeping the canvas point under
/// `(cursor_x, cursor_y)` (viewport px) pinned — the CapCut/Figma feel where
/// the spot under the cursor doesn't slide. Pan measures the content center
/// from the viewport center, so a point at offset `v` from the viewport center
/// scales about the pan: `pan' = v - (v - pan) * factor`.
#[allow(clippy::too_many_arguments)]
pub fn zoom_to(
    canvas_w: f32,
    canvas_h: f32,
    view_w: f32,
    view_h: f32,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    cursor_x: f32,
    cursor_y: f32,
    target_zoom: f32,
) -> PreviewView {
    let new_zoom = clamp_zoom(target_zoom);
    let factor = if zoom > 0.0 { new_zoom / zoom } else { 1.0 };
    let vx = cursor_x - view_w / 2.0;
    let vy = cursor_y - view_h / 2.0;
    let pan_x = vx - (vx - pan_x) * factor;
    let pan_y = vy - (vy - pan_y) * factor;
    clamp_view(canvas_w, canvas_h, view_w, view_h, new_zoom, pan_x, pan_y)
}

/// Pan by a viewport-px delta (two-finger scroll / drag), clamped to range.
#[allow(clippy::too_many_arguments)]
pub fn pan_by(
    canvas_w: f32,
    canvas_h: f32,
    view_w: f32,
    view_h: f32,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    dx: f32,
    dy: f32,
) -> PreviewView {
    clamp_view(
        canvas_w,
        canvas_h,
        view_w,
        view_h,
        zoom,
        pan_x + dx,
        pan_y + dy,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1920×1080 canvas in a matching-aspect viewport at half size: fit scale
    // 0.5, no letterbox, so fitted content is the full 960×540 viewport.
    const CW: f32 = 1920.0;
    const CH: f32 = 1080.0;
    const VW: f32 = 960.0;
    const VH: f32 = 540.0;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "expected {b}, got {a}");
    }

    #[test]
    fn zoom_clamps_into_range() {
        // Below the floor and above the ceiling both saturate.
        approx(clamp_view(CW, CH, VW, VH, 0.0001, 0.0, 0.0).zoom, MIN_ZOOM);
        approx(clamp_view(CW, CH, VW, VH, 999.0, 0.0, 0.0).zoom, MAX_ZOOM);
        // A non-finite zoom (degenerate divide upstream) falls back to fit.
        approx(
            clamp_view(CW, CH, VW, VH, f32::NAN, 0.0, 0.0).zoom,
            FIT_ZOOM,
        );
    }

    #[test]
    fn fit_and_zoomed_out_stay_centered() {
        // At fit, content == viewport ⇒ no pan room; any requested pan is
        // pulled back to center.
        let v = clamp_view(CW, CH, VW, VH, 1.0, 200.0, -200.0);
        approx(v.pan_x, 0.0);
        approx(v.pan_y, 0.0);
        // Zoomed out below fit, the frame is smaller than the viewport ⇒ still
        // centered, can't be scrolled into the dead space.
        let v = clamp_view(CW, CH, VW, VH, 0.5, 50.0, 50.0);
        approx(v.zoom, 0.5);
        approx(v.pan_x, 0.0);
        approx(v.pan_y, 0.0);
    }

    #[test]
    fn pan_clamps_to_the_zoomed_content_edges() {
        // At 2× the content is 1920×1080 in a 960×540 viewport: half the
        // overflow is (1920-960)/2 = 480 px horizontally, 270 px vertically.
        let v = pan_by(CW, CH, VW, VH, 2.0, 0.0, 0.0, 10000.0, 10000.0);
        approx(v.pan_x, 480.0);
        approx(v.pan_y, 270.0);
        let v = pan_by(CW, CH, VW, VH, 2.0, 0.0, 0.0, -10000.0, -10000.0);
        approx(v.pan_x, -480.0);
        approx(v.pan_y, -270.0);
        // A small pan well within range is taken verbatim.
        let v = pan_by(CW, CH, VW, VH, 2.0, 0.0, 0.0, 30.0, -40.0);
        approx(v.pan_x, 30.0);
        approx(v.pan_y, -40.0);
    }

    #[test]
    fn zoom_keeps_the_cursor_point_pinned() {
        // Map a viewport point through the same letterbox mapping the
        // hit-test uses, before and after the zoom; the canvas point under
        // the cursor must land back under the cursor.
        let cursor = (720.0, 405.0); // arbitrary off-center point
        let before = clamp_view(CW, CH, VW, VH, 1.0, 0.0, 0.0);
        let after = zoom_to(
            CW,
            CH,
            VW,
            VH,
            before.zoom,
            before.pan_x,
            before.pan_y,
            cursor.0,
            cursor.1,
            3.0,
        );

        // Canvas point under the cursor before the zoom.
        let (s0, ox0, oy0) = crate::preview_select::viewport_mapping(
            CW,
            CH,
            VW,
            VH,
            before.zoom,
            before.pan_x,
            before.pan_y,
        );
        let canvas_pt = ((cursor.0 - ox0) / s0, (cursor.1 - oy0) / s0);
        // Where that canvas point sits after the zoom.
        let (s1, ox1, oy1) = crate::preview_select::viewport_mapping(
            CW,
            CH,
            VW,
            VH,
            after.zoom,
            after.pan_x,
            after.pan_y,
        );
        approx(ox1 + canvas_pt.0 * s1, cursor.0);
        approx(oy1 + canvas_pt.1 * s1, cursor.1);
    }

    #[test]
    fn zoom_anchor_respects_pan_clamp() {
        // Zooming anchored at the extreme corner can't push pan past the
        // legal range for the new zoom.
        let v = zoom_to(CW, CH, VW, VH, 1.0, 0.0, 0.0, VW, VH, 2.0);
        assert!(
            v.pan_x.abs() <= 480.0 + 1e-3,
            "pan_x {} out of range",
            v.pan_x
        );
        assert!(
            v.pan_y.abs() <= 270.0 + 1e-3,
            "pan_y {} out of range",
            v.pan_y
        );
    }

    #[test]
    fn letterboxed_viewport_limits_pan_per_axis() {
        // 1920×1080 canvas in a square 1000×1000 viewport: fit scale 1000/1920,
        // fitted 1000×562.5. At fit the wide axis is full-bleed (no h-pan) and
        // the tall axis is letterboxed (no v-pan).
        let v = pan_by(CW, CH, 1000.0, 1000.0, 1.0, 0.0, 0.0, 500.0, 500.0);
        approx(v.pan_x, 0.0);
        approx(v.pan_y, 0.0);
        // At 2×: content 2000×1125 ⇒ h-room (2000-1000)/2 = 500, v-room
        // (1125-1000)/2 = 62.5.
        let v = pan_by(CW, CH, 1000.0, 1000.0, 2.0, 0.0, 0.0, 9999.0, 9999.0);
        approx(v.pan_x, 500.0);
        approx(v.pan_y, 62.5);
    }
}
