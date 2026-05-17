//! [`Renderer`]: wgpu device, YUV/RGB pipelines, upload, draw, readback.

use std::borrow::Cow;
use std::marker::PhantomData;

use decoder::{DecodedVideoFrame, FrameData, PixelFormat};

use crate::error::RendererError;
use crate::layer::Layer;
use crate::pixel_format;
use crate::target::RenderTarget;
use crate::upload;

const YUV420P_WGSL: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/yuv420p.wgsl"));
const NV12_WGSL: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/nv12.wgsl"));
const RGBA8_WGSL: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/rgba8.wgsl"));

fn shader_module(device: &wgpu::Device, label: &'static str, source: &'static str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(source)),
    })
}

fn linear_clamp_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("cutlass_linear_clamp"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

fn bind_layout_textures_plus_sampler(
    device: &wgpu::Device,
    label: &'static str,
    texture_bindings: &[u32],
    sampler_binding: u32,
) -> wgpu::BindGroupLayout {
    let mut entries = Vec::with_capacity(texture_bindings.len() + 1);
    for &b in texture_bindings {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                view_dimension: wgpu::TextureViewDimension::D2,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
            },
            count: None,
        });
    }
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: sampler_binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    });
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    })
}

fn pipeline_fullscreen(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    label: &'static str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Owns a wgpu [`wgpu::Device`] + [`wgpu::Queue`], three input-format pipelines, and draws into a caller-owned [`RenderTarget`](crate::RenderTarget).
///
/// # Threading
/// `Renderer` is [`Send`] but intentionally **not** [`Sync`]: use one renderer per consumer thread
/// (see `docs/renderer/research.md`).
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    sampler: wgpu::Sampler,
    pipeline_yuv420p: wgpu::RenderPipeline,
    pipeline_nv12: wgpu::RenderPipeline,
    pipeline_rgba8: wgpu::RenderPipeline,
    layout_yuv420p: wgpu::BindGroupLayout,
    layout_nv12: wgpu::BindGroupLayout,
    layout_rgba8: wgpu::BindGroupLayout,
    _not_sync: PhantomData<std::cell::Cell<()>>,
}

impl Renderer {
    /// Blocking adapter/device init (uses `pollster` internally).
    pub fn new() -> Result<Self, RendererError> {
        pollster::block_on(async {
            let mut inst_desc = wgpu::InstanceDescriptor::new_without_display_handle();
            inst_desc.backends = wgpu::Backends::all();
            let instance = wgpu::Instance::new(inst_desc);
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .map_err(|_| RendererError::NoAdapter)?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("cutlass_renderer"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    experimental_features: Default::default(),
                    memory_hints: Default::default(),
                    trace: wgpu::Trace::Off,
                })
                .await?;
            let sampler = linear_clamp_sampler(&device);

            let sm_yuv = shader_module(&device, "yuv420p", YUV420P_WGSL);
            let sm_nv12 = shader_module(&device, "nv12", NV12_WGSL);
            let sm_rgba = shader_module(&device, "rgba8", RGBA8_WGSL);

            let layout_yuv420p =
                bind_layout_textures_plus_sampler(&device, "layout_yuv420p", &[0, 1, 2], 3);
            let layout_nv12 = bind_layout_textures_plus_sampler(&device, "layout_nv12", &[0, 1], 2);
            let layout_rgba8 = bind_layout_textures_plus_sampler(&device, "layout_rgba8", &[0], 1);

            let pl_yuv = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pl_yuv420p"),
                bind_group_layouts: &[Some(&layout_yuv420p)],
                immediate_size: 0,
            });
            let pl_nv12 = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pl_nv12"),
                bind_group_layouts: &[Some(&layout_nv12)],
                immediate_size: 0,
            });
            let pl_rgba = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pl_rgba8"),
                bind_group_layouts: &[Some(&layout_rgba8)],
                immediate_size: 0,
            });

            let pipeline_yuv420p = pipeline_fullscreen(&device, &pl_yuv, &sm_yuv, "pipe_yuv420p");
            let pipeline_nv12 = pipeline_fullscreen(&device, &pl_nv12, &sm_nv12, "pipe_nv12");
            let pipeline_rgba8 = pipeline_fullscreen(&device, &pl_rgba, &sm_rgba, "pipe_rgba8");

            Ok(Self {
                device,
                queue,
                sampler,
                pipeline_yuv420p,
                pipeline_nv12,
                pipeline_rgba8,
                layout_yuv420p,
                layout_nv12,
                layout_rgba8,
                _not_sync: PhantomData,
            })
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Renders the single [`Layer`] into `target` (MVP: `layers.len() == 1`, dimensions must match).
    pub fn render(&mut self, layers: &[Layer], target: &RenderTarget) -> Result<(), RendererError> {
        if layers.len() != 1 {
            return Err(RendererError::UnsupportedLayerCount {
                count: layers.len(),
            });
        }
        let layer = &layers[0];
        let frame = &layer.frame;
        if target.width != frame.width || target.height != frame.height {
            return Err(RendererError::TargetSizeMismatch {
                target_w: target.width,
                target_h: target.height,
                frame_w: frame.width,
                frame_h: frame.height,
            });
        }
        if frame.width == 0 || frame.height == 0 {
            return Err(RendererError::ZeroDimension);
        }
        let cpu = match &frame.data {
            FrameData::Cpu(c) => c,
            _ => return Err(RendererError::NonCpuFrame),
        };
        let fmt = cpu.format;
        if !pixel_format::plane_count_matches(fmt, cpu.planes.len()) {
            return Err(RendererError::BadFrameLayout);
        }

        let textures = upload::upload_cpu_frame(
            &self.device,
            &self.queue,
            fmt,
            frame.width,
            frame.height,
            &cpu.planes,
        )?;

        let bind_group = match fmt {
            PixelFormat::Yuv420p => {
                let v0 = textures[0].create_view(&wgpu::TextureViewDescriptor::default());
                let v1 = textures[1].create_view(&wgpu::TextureViewDescriptor::default());
                let v2 = textures[2].create_view(&wgpu::TextureViewDescriptor::default());
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("bg_yuv420p"),
                    layout: &self.layout_yuv420p,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&v0),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&v1),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&v2),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                })
            }
            PixelFormat::Nv12 => {
                let v0 = textures[0].create_view(&wgpu::TextureViewDescriptor::default());
                let v1 = textures[1].create_view(&wgpu::TextureViewDescriptor::default());
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("bg_nv12"),
                    layout: &self.layout_nv12,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&v0),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&v1),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                })
            }
            PixelFormat::Rgba8 => {
                let v0 = textures[0].create_view(&wgpu::TextureViewDescriptor::default());
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("bg_rgba8"),
                    layout: &self.layout_rgba8,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&v0),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                })
            }
            _ => return Err(RendererError::UnsupportedFormat(fmt)),
        };

        let pipeline: &wgpu::RenderPipeline = match fmt {
            PixelFormat::Yuv420p => &self.pipeline_yuv420p,
            PixelFormat::Nv12 => &self.pipeline_nv12,
            PixelFormat::Rgba8 => &self.pipeline_rgba8,
            _ => return Err(RendererError::UnsupportedFormat(fmt)),
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cutlass_render_pass"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cutlass_main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        // Textures dropped here after submit — acceptable: GPU keeps them until commands complete.
        Ok(())
    }

    /// Copies `target` to CPU RGBA8 (`width * height * 4`, tightly packed rows).
    pub fn read_pixels_rgba8(&self, target: &RenderTarget) -> Result<Vec<u8>, RendererError> {
        if target.width == 0 || target.height == 0 {
            return Err(RendererError::ZeroDimension);
        }
        let bytes_per_pixel = 4u32;
        let tight_row = target
            .width
            .checked_mul(bytes_per_pixel)
            .ok_or(RendererError::BadFrameLayout)?;
        let padded_row = upload::align_up(tight_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer_size =
            u64::from(padded_row).checked_mul(u64::from(target.height)).ok_or(RendererError::BadFrameLayout)?;
        let read_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cutlass_readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cutlass_readback_copy"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &read_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(target.height),
                },
            },
            wgpu::Extent3d {
                width: target.width,
                height: target.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RendererError::Readback(e.to_string()))?;

        let slice = read_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RendererError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|_| RendererError::Readback("map channel closed".into()))?
            .map_err(|e| RendererError::Readback(e.to_string()))?;

        let mapped = slice.get_mapped_range();
        let mut out =
            Vec::with_capacity((tight_row * target.height) as usize);
        for row in 0..target.height as usize {
            let src_start = row * padded_row as usize;
            let src_end = src_start + tight_row as usize;
            out.extend_from_slice(&mapped[src_start..src_end]);
        }
        drop(mapped);
        read_buffer.unmap();
        Ok(out)
    }
}

/// Uploads a decoded frame and returns GPU textures (tests / inspection).
pub fn upload_decoded_frame_for_test(
    renderer: &Renderer,
    frame: &DecodedVideoFrame,
) -> Result<Vec<wgpu::Texture>, RendererError> {
    let cpu = match &frame.data {
        FrameData::Cpu(c) => c,
        _ => return Err(RendererError::NonCpuFrame),
    };
    upload::upload_cpu_frame(
        renderer.device(),
        renderer.queue(),
        cpu.format,
        frame.width,
        frame.height,
        &cpu.planes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RendererError;
    use crate::layer::{Layer, Transform};
    use crate::target::RenderTarget;
    use decoder::{CpuFrame, FrameData, Plane, PixelFormat, Rational};

    #[test]
    fn yuv420p_wgsl_contains_bt709_limited_range_coefficients() {
        assert!(YUV420P_WGSL.contains("1.5748"), "BT.709 R row");
        assert!(YUV420P_WGSL.contains("219.0"), "limited-range Y scale");
        assert!(NV12_WGSL.contains("224.0"), "limited-range chroma scale");
    }

    #[test]
    fn renderer_new_succeeds() {
        let _ = Renderer::new().expect("renderer");
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn renderer_is_send() {
        assert_send::<Renderer>();
    }

    fn synthetic_yuv420(y: u8, u: u8, v: u8, w: u32, h: u32) -> DecodedVideoFrame {
        let hw = (w * h) as usize;
        let ch = (w / 2 * h / 2) as usize;
        DecodedVideoFrame {
            width: w,
            height: h,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 30_000),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Yuv420p,
                planes: vec![
                    Plane {
                        data: vec![y; hw],
                        stride: w as usize,
                    },
                    Plane {
                        data: vec![u; ch],
                        stride: (w / 2) as usize,
                    },
                    Plane {
                        data: vec![v; ch],
                        stride: (w / 2) as usize,
                    },
                ],
            }),
        }
    }

    #[test]
    fn render_yuv420p_solid_mid_gray_produces_mid_gray_rgb() {
        let mut r = Renderer::new().expect("renderer");
        let w = 64u32;
        let h = 64u32;
        // Y ≈ 125 → RGB ≈ 127 after BT.709 limited expansion (see research.md).
        let frame = synthetic_yuv420(125, 128, 128, w, h);
        let target = RenderTarget::new(r.device(), w, h);
        let layer = Layer {
            frame,
            transform: Transform::identity(),
            opacity: 1.0,
        };
        r.render(&[layer], &target).expect("render");
        let px = r.read_pixels_rgba8(&target).expect("read");
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        let (red, green, blue) = (px[i], px[i + 1], px[i + 2]);
        for c in [red, green, blue] {
            assert!(
                (127isize - c as isize).abs() <= 2,
                "expected ~127, got r={red} g={green} b={blue}"
            );
        }
    }

    #[test]
    fn render_yuv420p_solid_black_produces_black_rgb() {
        let mut r = Renderer::new().expect("renderer");
        let frame = synthetic_yuv420(16, 128, 128, 32, 32);
        let target = RenderTarget::new(r.device(), 32, 32);
        r.render(
            &[Layer {
                frame,
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &target,
        )
        .expect("render");
        let px = r.read_pixels_rgba8(&target).expect("read");
        for chunk in px.chunks_exact(4) {
            assert!(
                chunk[0] <= 8 && chunk[1] <= 8 && chunk[2] <= 8,
                "expected near-black RGB, got {:?}",
                &chunk[0..3]
            );
        }
    }

    #[test]
    fn render_yuv420p_solid_white_produces_white_rgb() {
        let mut r = Renderer::new().expect("renderer");
        let frame = synthetic_yuv420(235, 128, 128, 32, 32);
        let target = RenderTarget::new(r.device(), 32, 32);
        r.render(
            &[Layer {
                frame,
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &target,
        )
        .expect("render");
        let px = r.read_pixels_rgba8(&target).expect("read");
        let i = ((16 * 32 + 16) * 4) as usize;
        for c in 0..3 {
            assert!(px[i + c] >= 248, "expected near-white channel {}", c);
        }
    }

    #[test]
    fn read_pixels_rejects_zero_sized_target_dimensions() {
        let r = Renderer::new().expect("renderer");
        let mut target = RenderTarget::new(r.device(), 4, 4);
        target.width = 0;
        assert!(matches!(
            r.read_pixels_rgba8(&target),
            Err(RendererError::ZeroDimension)
        ));
        target.width = 4;
        target.height = 0;
        assert!(matches!(
            r.read_pixels_rgba8(&target),
            Err(RendererError::ZeroDimension)
        ));
    }

    #[test]
    fn readback_tightly_packed_row_length_for_odd_width() {
        let r = Renderer::new().expect("renderer");
        let w = 17u32;
        let h = 3u32;
        let target = RenderTarget::new(r.device(), w, h);
        let px = r.read_pixels_rgba8(&target).expect("read empty target");
        assert_eq!(px.len(), (w * h * 4) as usize);
    }

    #[test]
    fn render_rejects_no_layers() {
        let mut r = Renderer::new().expect("renderer");
        let target = RenderTarget::new(r.device(), 8, 8);
        assert!(matches!(
            r.render(&[], &target),
            Err(RendererError::UnsupportedLayerCount { count: 0 })
        ));
    }

    #[test]
    fn render_rejects_two_layers_in_mvp() {
        let mut r = Renderer::new().expect("renderer");
        let f = synthetic_yuv420(125, 128, 128, 8, 8);
        let target = RenderTarget::new(r.device(), 8, 8);
        let layer = Layer {
            frame: f.clone(),
            transform: Transform::identity(),
            opacity: 1.0,
        };
        assert!(matches!(
            r.render(&[layer.clone(), layer], &target),
            Err(RendererError::UnsupportedLayerCount { count: 2 })
        ));
    }

    #[test]
    fn render_rejects_target_size_mismatch() {
        let mut r = Renderer::new().expect("renderer");
        let frame = synthetic_yuv420(125, 128, 128, 8, 8);
        let target = RenderTarget::new(r.device(), 9, 8);
        let err = r
            .render(
                &[Layer {
                    frame,
                    transform: Transform::identity(),
                    opacity: 1.0,
                }],
                &target,
            )
            .expect_err("mismatch");
        assert!(matches!(
            err,
            RendererError::TargetSizeMismatch {
                target_w: 9,
                target_h: 8,
                frame_w: 8,
                frame_h: 8,
            }
        ));
    }

    #[test]
    fn render_rejects_zero_sized_frame() {
        let mut r = Renderer::new().expect("renderer");
        let mut frame = synthetic_yuv420(16, 128, 128, 4, 4);
        frame.width = 0;
        frame.height = 0;
        let mut target = RenderTarget::new(r.device(), 4, 4);
        target.width = 0;
        target.height = 0;
        assert!(matches!(
            r.render(
                &[Layer {
                    frame,
                    transform: Transform::identity(),
                    opacity: 1.0,
                }],
                &target
            ),
            Err(RendererError::ZeroDimension)
        ));
    }

    #[test]
    fn render_rejects_wrong_plane_count_for_format() {
        let mut r = Renderer::new().expect("renderer");
        let w = 8u32;
        let h = 8u32;
        let frame = DecodedVideoFrame {
            width: w,
            height: h,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 30),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Yuv420p,
                planes: vec![
                    Plane {
                        data: vec![128u8; (w * h) as usize],
                        stride: w as usize,
                    },
                    Plane {
                        data: vec![128u8; (w / 2 * h / 2) as usize],
                        stride: (w / 2) as usize,
                    },
                ],
            }),
        };
        let target = RenderTarget::new(r.device(), w, h);
        assert!(matches!(
            r.render(
                &[Layer {
                    frame,
                    transform: Transform::identity(),
                    opacity: 1.0,
                }],
                &target
            ),
            Err(RendererError::BadFrameLayout)
        ));
    }

    #[test]
    fn identical_synthetic_render_is_deterministic_byte_for_byte() {
        let mut r = Renderer::new().expect("renderer");
        let frame = synthetic_yuv420(90, 100, 220, 16, 16);
        let target = RenderTarget::new(r.device(), 16, 16);
        r.render(
            &[Layer {
                frame: frame.clone(),
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &target,
        )
        .expect("a");
        let a = r.read_pixels_rgba8(&target).expect("ra");
        r.render(
            &[Layer {
                frame,
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &target,
        )
        .expect("b");
        let b = r.read_pixels_rgba8(&target).expect("rb");
        assert_eq!(a, b);
    }

    #[test]
    fn rgba8_upload_uses_first_row_bytes_not_trailing_stride_garbage() {
        let w = 2u32;
        let h = 2u32;
        let stride = 32usize;
        let mut data = vec![0xffu8; stride * h as usize];
        data[0] = 10;
        data[1] = 20;
        data[2] = 30;
        data[3] = 40;
        data[4] = 1;
        data[5] = 2;
        data[6] = 3;
        data[7] = 4;
        data[stride] = 50;
        data[stride + 1] = 60;
        data[stride + 2] = 70;
        data[stride + 3] = 80;
        data[stride + 4] = 5;
        data[stride + 5] = 6;
        data[stride + 6] = 7;
        data[stride + 7] = 8;
        data[stride + 8] = 99;
        data[stride + 9] = 99;
        let frame = DecodedVideoFrame {
            width: w,
            height: h,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 1),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Rgba8,
                planes: vec![Plane { stride, data }],
            }),
        };
        let mut r = Renderer::new().expect("renderer");
        let target = RenderTarget::new(r.device(), w, h);
        r.render(
            &[Layer {
                frame: frame.clone(),
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &target,
        )
        .expect("render");
        let px = r.read_pixels_rgba8(&target).expect("read");
        let cpu = match &frame.data {
            FrameData::Cpu(c) => &c.planes[0],
            _ => panic!(),
        };
        let mut expected = Vec::with_capacity((w * h * 4) as usize);
        expected.extend_from_slice(&cpu.data[0..(w * 4) as usize]);
        expected.extend_from_slice(&cpu.data[cpu.stride..cpu.stride + (w * 4) as usize]);
        assert_eq!(px, expected);
        assert_eq!(&px[0..4], &[10, 20, 30, 40]);
        assert_eq!(&px[4..8], &[1, 2, 3, 4]);
    }

    #[test]
    fn yuv420p_high_v_produces_redder_than_blue_center_pixel() {
        let mut r = Renderer::new().expect("renderer");
        // Neutral chroma U; elevated V pushes red channel via BT.709 matrix.
        let frame = synthetic_yuv420(180, 128, 200, 48, 48);
        let target = RenderTarget::new(r.device(), 48, 48);
        r.render(
            &[Layer {
                frame,
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &target,
        )
        .expect("render");
        let px = r.read_pixels_rgba8(&target).expect("read");
        let i = ((24 * 48 + 24) * 4) as usize;
        let (pr, pg, pb) = (px[i] as i16, px[i + 1] as i16, px[i + 2] as i16);
        assert!(
            pr > pb + 15,
            "expected red > blue for warm chroma, got R={pr} G={pg} B={pb}"
        );
    }

    #[test]
    fn upload_decoded_frame_for_test_errors_on_plane_layout() {
        let r = Renderer::new().expect("renderer");
        let w = 4u32;
        let h = 4u32;
        let frame = DecodedVideoFrame {
            width: w,
            height: h,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 1),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Yuv420p,
                planes: vec![Plane {
                    data: vec![0u8; 1],
                    stride: 1,
                }],
            }),
        };
        assert!(matches!(
            upload_decoded_frame_for_test(&r, &frame),
            Err(RendererError::BadFrameLayout)
        ));
    }

    #[test]
    fn mvp_ignores_non_identity_transform_pixels_match_identity() {
        let mut r = Renderer::new().expect("renderer");
        let frame = synthetic_yuv420(100, 110, 120, 24, 24);
        let t0 = RenderTarget::new(r.device(), 24, 24);
        r.render(
            &[Layer {
                frame: frame.clone(),
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &t0,
        )
        .expect("id");
        let p0 = r.read_pixels_rgba8(&t0).expect("r0");
        let t1 = RenderTarget::new(r.device(), 24, 24);
        r.render(
            &[Layer {
                frame,
                transform: Transform {
                    translate: [12.0, -7.0],
                    scale: [0.25, 4.0],
                    rotate_radians: 1.57,
                },
                opacity: 0.25,
            }],
            &t1,
        )
        .expect("tr");
        let p1 = r.read_pixels_rgba8(&t1).expect("r1");
        assert_eq!(p0, p1, "MVP must ignore transform/opacity (same framebuffer geometry)");
    }
}
