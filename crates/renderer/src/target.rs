/// Output RGBA8 target. Caller allocates and owns the texture; [`crate::Renderer::render`] draws into it.
pub struct RenderTarget {
    pub width: u32,
    pub height: u32,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

impl RenderTarget {
    /// Creates an `Rgba8Unorm` texture sized `width × height` with `RENDER_ATTACHMENT` + `COPY_SRC`.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cutlass_render_target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            width,
            height,
            texture,
            view,
        }
    }
}
