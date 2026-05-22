mod ruler;
mod timecode;
mod timeline;

slint::include_modules!();
use slint::BackendSelector;
use slint::wgpu_28::WGPUConfiguration;

fn main() -> Result<(), slint::PlatformError> {
    BackendSelector::new()
        .require_wgpu_28(WGPUConfiguration::default())
        .select()?;

    let app = AppWindow::new()?;
    let fullscreen_preview = FullscreenPreview::new()?;
    app.window().set_maximized(true);
    app.global::<TimelineLib>()
        .on_sequence_duration(timeline::sequence_duration);

    let fullscreen_preview_for_enter = fullscreen_preview.as_weak();
    app.global::<TimelineViewState>().on_enter_fullscreen(move || {
        if let Some(fullscreen_preview) = fullscreen_preview_for_enter.upgrade() {
            let _ = fullscreen_preview.show();
        }
    });

    let fullscreen_preview_for_exit = fullscreen_preview.as_weak();
    app.global::<TimelineViewState>().on_exit_fullscreen(move || {
        if let Some(fullscreen_preview) = fullscreen_preview_for_exit.upgrade() {
            let _ = fullscreen_preview.hide();
        }
    });

    // Install the ruler tick generator. Slint will invoke this whenever
    // any of the dependent properties (scroll-x, viewport width, zoom,
    // fps, drop-frame) change — see `ui/lib/ruler-backend.slint` for
    // the contract and `ui/panels/timeline/ruler.slint` for the call site.
    app.global::<RulerBackend>().on_ticks(
        |scroll_x, viewport_w, zoom, fps_num, fps_den, drop_frame| {
            ruler::ticks_model(scroll_x, viewport_w, zoom, fps_num, fps_den, drop_frame)
        },
    );

    app.run()
}
