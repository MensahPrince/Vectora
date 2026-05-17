//! Prints the default wgpu adapter (Metal on macOS when available).

fn main() {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("adapter");
    let info = adapter.get_info();
    println!(
        "adapter: {} ({:?}) backend={:?}",
        info.name, info.device_type, info.backend
    );
}
