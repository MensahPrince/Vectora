//! OS file-drop support: cursor position at drop time + placement planning.
//!
//! winit 0.30's `DroppedFile` event carries no pointer position (the
//! position-carrying `DragDropped` API is winit 0.31), so the drop handler
//! asks the OS for the global cursor position instead and converts it into
//! the window's logical coordinate space — the same space Slint's
//! `absolute-position` values live in. One `cfg`-gated backend per platform:
//! `NSEvent.mouseLocation` (macOS), `GetCursorPos` (Windows), and
//! `XQueryPointer` (X11). Wayland has no global pointer query — but winit
//! 0.30 never delivers file drops there either, so the question doesn't
//! arise; [`cursor_in_window`] returns `None` and callers fall back to the
//! pool-only import.
//!
//! The placement planner ([`plan_sequential_starts`], [`insertion_boundary`])
//! is pure tick math shared with the worker's multi-file drop handler so the
//! end-to-end arrangement is unit-testable without an engine.

use slint::winit_030::winit;

/// The global cursor position in `window`'s logical coordinate space
/// (top-left origin, scale-factor points — Slint window coordinates), or
/// `None` where the platform can't answer (Wayland, missing handle).
pub fn cursor_in_window(window: &winit::window::Window) -> Option<(f32, f32)> {
    platform::cursor_in_window(window)
}

/// Start ticks that place clips of `durations` end-to-end from `anchor`,
/// sliding right past `spans` — the sorted, non-overlapping `[start, end)`
/// ranges already occupied on the landing lane. Each clip starts at the
/// previous one's end (clip A end → clip B start); a start that would overlap
/// an existing clip first-fit slides to the blocker's end, and every later
/// clip chains from wherever the slide landed. O(files + clips).
pub fn plan_sequential_starts(spans: &[(i64, i64)], anchor: i64, durations: &[i64]) -> Vec<i64> {
    let mut starts = Vec::with_capacity(durations.len());
    let mut cursor = anchor.max(0);
    for &duration in durations {
        let duration = duration.max(1);
        let mut start = cursor;
        for &(s, e) in spans {
            if start + duration <= s {
                break; // fits entirely before this clip
            }
            start = start.max(e);
        }
        starts.push(start);
        cursor = start + duration;
    }
    starts
}

/// The main-track-magnet insertion boundary for content whose left edge sits
/// at `desired`: the start of the first clip whose midpoint lies right of it
/// (CapCut feel — crossing a clip's middle flips the caret to its other
/// side), else the lane's content end. Mirrors `snap::resolve_insertion` so
/// worker-side OS drops land where the library-drag caret would.
pub fn insertion_boundary(spans: &[(i64, i64)], desired: i64) -> i64 {
    let desired = desired.max(0);
    for &(s, e) in spans {
        if desired < s + (e - s) / 2 {
            return s;
        }
    }
    spans.iter().map(|&(_, e)| e).max().unwrap_or(0)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::winit;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSEvent, NSView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub fn cursor_in_window(window: &winit::window::Window) -> Option<(f32, f32)> {
        let raw = window.window_handle().ok()?.as_raw();
        let RawWindowHandle::AppKit(handle) = raw else {
            return None;
        };
        // SAFETY: `handle.ns_view` is documented by raw-window-handle to be a
        // live, retained `NSView *` while the WindowHandle is borrowed from
        // winit; retaining gives us an independently owned reference (same
        // pattern as window.rs).
        let view: Retained<NSView> = unsafe { Retained::retain(handle.ns_view.as_ptr().cast())? };
        let ns_window = view.window()?;
        // Screen (bottom-left origin) → window base → view coordinates. All
        // in logical points; winit's view is flipped (top-left origin), which
        // matches Slint's window space — flip by hand if it ever isn't.
        let in_window = ns_window.convertPointFromScreen(NSEvent::mouseLocation());
        let in_view = view.convertPoint_fromView(in_window, None);
        let y = if view.isFlipped() {
            in_view.y
        } else {
            view.bounds().size.height - in_view.y
        };
        Some((in_view.x as f32, y as f32))
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::winit;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    pub fn cursor_in_window(window: &winit::window::Window) -> Option<(f32, f32)> {
        let mut point = POINT::default();
        // SAFETY: `point` is a valid out-param for the duration of the call.
        unsafe { GetCursorPos(&mut point) }.ok()?;
        // Screen physical px → client-area physical px → logical points.
        let origin = window.inner_position().ok()?;
        let scale = window.scale_factor();
        Some((
            ((f64::from(point.x) - f64::from(origin.x)) / scale) as f32,
            ((f64::from(point.y) - f64::from(origin.y)) / scale) as f32,
        ))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::winit;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::RefCell;
    use x11rb::protocol::xproto::ConnectionExt;
    use x11rb::rust_connection::RustConnection;

    thread_local! {
        // One X connection per (UI) thread, opened lazily on the first drop
        // and reused across the 30 Hz hover polls. `None` after a failed
        // connect is retried — X can come up after the app on XWayland.
        static X_CONN: RefCell<Option<RustConnection>> = const { RefCell::new(None) };
    }

    pub fn cursor_in_window(window: &winit::window::Window) -> Option<(f32, f32)> {
        let raw = window.window_handle().ok()?.as_raw();
        // Wayland has no global pointer query (and winit 0.30 delivers no
        // file-drop events there anyway) — only X11 handles resolve.
        let xid: u32 = match raw {
            RawWindowHandle::Xlib(handle) => handle.window as u32,
            RawWindowHandle::Xcb(handle) => handle.window.get(),
            _ => return None,
        };
        X_CONN.with(|slot| {
            let mut conn = slot.borrow_mut();
            if conn.is_none() {
                *conn = x11rb::connect(None).ok().map(|(c, _)| c);
            }
            let reply = conn
                .as_ref()?
                .query_pointer(xid)
                .ok()
                .and_then(|c| c.reply().ok());
            let Some(reply) = reply else {
                // Drop a connection that stopped answering; the next drop
                // reconnects.
                *conn = None;
                return None;
            };
            // `win_x/win_y` are already relative to the queried window, in
            // physical px.
            let scale = window.scale_factor();
            Some((
                (f64::from(reply.win_x) / scale) as f32,
                (f64::from(reply.win_y) / scale) as f32,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_lane_places_end_to_end_from_the_anchor() {
        let starts = plan_sequential_starts(&[], 100, &[50, 30, 20]);
        assert_eq!(starts, vec![100, 150, 180]);
    }

    #[test]
    fn negative_anchor_clamps_to_zero() {
        let starts = plan_sequential_starts(&[], -40, &[10, 10]);
        assert_eq!(starts, vec![0, 10]);
    }

    #[test]
    fn blocked_anchor_slides_the_whole_chain_right() {
        // Lane occupied over [80, 200): a drop at 100 slides to 200 and the
        // rest chain from there.
        let starts = plan_sequential_starts(&[(80, 200)], 100, &[50, 25]);
        assert_eq!(starts, vec![200, 250]);
    }

    #[test]
    fn chain_fits_a_gap_then_slides_past_the_next_clip() {
        // Gap [0, 100), clip [100, 300): the first fits before the clip, the
        // second collides mid-chain and slides to the clip's end.
        let starts = plan_sequential_starts(&[(100, 300)], 0, &[60, 60]);
        assert_eq!(starts, vec![0, 300]);
        // A chained clip that exactly abuts the blocker stays put.
        let starts = plan_sequential_starts(&[(100, 300)], 0, &[100, 60]);
        assert_eq!(starts, vec![0, 300]);
    }

    #[test]
    fn durations_are_clamped_to_at_least_one_tick() {
        let starts = plan_sequential_starts(&[], 0, &[0, 0]);
        assert_eq!(starts, vec![0, 1]);
    }

    #[test]
    fn insertion_boundary_flips_at_clip_midpoints() {
        let spans = [(0, 100), (100, 250)];
        // Left of the first clip's midpoint → before it.
        assert_eq!(insertion_boundary(&spans, 20), 0);
        // Past the first midpoint but left of the second → between them.
        assert_eq!(insertion_boundary(&spans, 80), 100);
        // Past every midpoint → after the last clip (content end).
        assert_eq!(insertion_boundary(&spans, 240), 250);
    }

    #[test]
    fn insertion_boundary_on_an_empty_lane_is_zero() {
        assert_eq!(insertion_boundary(&[], 500), 0);
    }
}
