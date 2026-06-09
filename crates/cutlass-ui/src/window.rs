// Helpers for driving the host window into a chromeless "fills the
// screen's visible area" state and back. Slint's `no-frame` toggle on
// its own only removes decorations — it does not grow the window to
// cover what the title bar used to occupy — so on macOS we drop down
// to the underlying `NSWindow` and resize it to `NSScreen.visibleFrame`
// (which already excludes the menu bar and dock). On other platforms
// we fall back to the monitor's full bounds via winit, which matches
// how desktop WMs handle maximized borderless windows.

use slint::winit_030::winit;

#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSView, NSWindow};
#[cfg(target_os = "macos")]
use objc2_foundation::NSRect;
#[cfg(target_os = "macos")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[derive(Clone, Copy)]
pub struct SavedFrame {
    #[cfg(target_os = "macos")]
    rect: NSRect,
    #[cfg(not(target_os = "macos"))]
    position: winit::dpi::PhysicalPosition<i32>,
    #[cfg(not(target_os = "macos"))]
    size: winit::dpi::PhysicalSize<u32>,
}

// Resolve the `NSWindow` hosting the given winit window through its
// raw `NSView` handle. Returns `None` if the handle isn't available
// (e.g. on a non-AppKit backend or before the window is shown).
#[cfg(target_os = "macos")]
fn ns_window_of(window: &winit::window::Window) -> Option<Retained<NSWindow>> {
    let raw = window.window_handle().ok()?.as_raw();
    let RawWindowHandle::AppKit(handle) = raw else {
        return None;
    };
    // SAFETY: `handle.ns_view` is documented by raw-window-handle to be
    // a live, retained `NSView *` for the duration the WindowHandle is
    // borrowed from winit. We retain it before reading `-window` so the
    // returned `NSWindow` is independently owned.
    unsafe {
        let view_ptr: *mut NSView = handle.ns_view.as_ptr().cast();
        let view: Retained<NSView> = Retained::retain(view_ptr)?;
        view.window()
    }
}

// Drop the OS frame and resize the window to cover the screen's
// visible area. Returns the previous frame so the caller can restore
// it via [`exit`].
pub fn enter_chromeless_maximized(window: &winit::window::Window) -> Option<SavedFrame> {
    #[cfg(target_os = "macos")]
    {
        let ns_window = ns_window_of(window)?;
        // SAFETY: `screen`, `frame` and `setFrame:display:` are all
        // safe to call on a valid retained `NSWindow` from the main
        // thread, which is where winit dispatches our callbacks.
        unsafe {
            let screen = ns_window.screen()?;
            let visible = screen.visibleFrame();
            let saved = SavedFrame {
                rect: ns_window.frame(),
            };
            window.set_decorations(false);
            ns_window.setFrame_display(visible, true);
            Some(saved)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let position = window.outer_position().ok()?;
        let size = window.outer_size();
        let saved = SavedFrame { position, size };
        let monitor = window.current_monitor()?;
        window.set_decorations(false);
        window.set_outer_position(monitor.position());
        let _ = window.request_inner_size(monitor.size());
        Some(saved)
    }
}

// Restore the frame captured by [`enter_chromeless_maximized`] and put
// the decorations back. No-ops if the saved frame is `None` (which
// happens if we never successfully entered the chromeless state).
pub fn exit_chromeless_maximized(window: &winit::window::Window, saved: SavedFrame) {
    window.set_decorations(true);
    #[cfg(target_os = "macos")]
    {
        if let Some(ns_window) = ns_window_of(window) {
            // SAFETY: see `enter_chromeless_maximized`.
            unsafe {
                ns_window.setFrame_display(saved.rect, true);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.set_outer_position(saved.position);
        let _ = window.request_inner_size(saved.size);
    }
}
