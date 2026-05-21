slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let app = AppWindow::new()?;
    // let app = TestApp::new()?;
    app.run()
}
