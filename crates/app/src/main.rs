slint::include_modules!();

use slint::BackendSelector;
use slint::wgpu_28::WGPUConfiguration;
use tracing_subscriber::EnvFilter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    BackendSelector::new()
        .require_wgpu_28(WGPUConfiguration::default())
        .select()?;

    let mut app = AppWindow::new()?;
    create_project(&mut app);
    app.run()?;
    Ok(())
}

fn create_project(app: &mut AppWindow) {
    let project = UiProject {
        id: "1".into(),
        name: "Project 1".into(),
        file_path: "project.cutlass".into(),
        schema: todo!(),
        sequences: todo!(),
        media_bin: todo!(),
        active_sequence_id: todo!(),
        is_dirty: todo!(),
    };

    app.global::<AppState>().set_project(project);
}
