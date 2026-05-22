mod ruler;
mod timecode;
mod timeline;

use std::cell::Cell;
use std::rc::Rc;

use slint::BackendSelector;
use slint::wgpu_28::WGPUConfiguration;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    BackendSelector::new()
        .require_wgpu_28(WGPUConfiguration::default())
        .select()?;

    let app = AppWindow::new()?;

    app.global::<TimelineLib>()
        .on_sequence_duration(timeline::sequence_duration);

    wire_preview_maximize(&app);

    app.run()
}

// Bridges the `AppState.{enter,exit}-preview-maximized` callbacks to
// the host window. Maximizing on enter only mutates window state if it
// wasn't already maximized, and exit only un-maximizes if *we* were the
// ones who maximized it — so the user's prior maximize state is
// preserved across a focus-mode round-trip.
fn wire_preview_maximize(app: &AppWindow) {
    let we_maximized = Rc::new(Cell::new(false));
    let app_weak = app.as_weak();

    app.global::<AppState>().on_enter_preview_maximized({
        let app_weak = app_weak.clone();
        let we_maximized = we_maximized.clone();
        move || {
            let Some(app) = app_weak.upgrade() else { return };
            let window = app.window();
            if !window.is_maximized() {
                window.set_maximized(true);
                we_maximized.set(true);
            }
            app.global::<AppState>().set_preview_maximized(true);
        }
    });

    app.global::<AppState>().on_exit_preview_maximized({
        let app_weak = app_weak.clone();
        let we_maximized = we_maximized.clone();
        move || {
            let Some(app) = app_weak.upgrade() else { return };
            if we_maximized.replace(false) {
                app.window().set_maximized(false);
            }
            app.global::<AppState>().set_preview_maximized(false);
        }
    });
}
