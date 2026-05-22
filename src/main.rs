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

    app.run()
}
