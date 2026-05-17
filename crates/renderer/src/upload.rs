use decoder::Plane;

use crate::error::RendererError;
use crate::pixel_format;

pub(crate) fn align_up(value: u32, alignment: u32) -> u32 {
    debug_assert!(alignment > 0);
    value.div_ceil(alignment) * alignment
}

/// Copies `row_bytes × height` from `plane` into a buffer whose rows are padded to `wgpu` copy alignment.
pub(crate) fn padded_plane_staging(
    plane: &Plane,
    row_bytes: u32,
    height: u32,
) -> Result<(Vec<u8>, u32), RendererError> {
    let padded_stride = align_up(row_bytes, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let need = usize::try_from(padded_stride)
        .ok()
        .and_then(|ps| ps.checked_mul(height as usize))
        .ok_or(RendererError::BadFrameLayout)?;
    let mut out = vec![0u8; need];
    let stride = plane.stride;
    let rb = row_bytes as usize;
    for row in 0..height as usize {
        let src_lo = row.checked_mul(stride).ok_or(RendererError::BadFrameLayout)?;
        let src_hi = src_lo.checked_add(rb).ok_or(RendererError::BadFrameLayout)?;
        if src_hi > plane.data.len() {
            return Err(RendererError::BadFrameLayout);
        }
        let dst_lo = row
            .checked_mul(padded_stride as usize)
            .ok_or(RendererError::BadFrameLayout)?;
        let dst_hi = dst_lo.checked_add(rb).ok_or(RendererError::BadFrameLayout)?;
        out[dst_lo..dst_hi].copy_from_slice(&plane.data[src_lo..src_hi]);
    }
    Ok((out, padded_stride))
}

pub(crate) fn upload_plane_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    padded: &[u8],
    bytes_per_row: u32,
    width: u32,
    height: u32,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        padded,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Builds textures for each plane and uploads. Returns `(textures, bind_resources)` — textures must live until after submit.
pub(crate) fn upload_cpu_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: decoder::PixelFormat,
    frame_width: u32,
    frame_height: u32,
    planes: &[Plane],
) -> Result<Vec<wgpu::Texture>, RendererError> {
    if !pixel_format::plane_count_matches(format, planes.len()) {
        return Err(RendererError::BadFrameLayout);
    }
    let mut textures = Vec::with_capacity(planes.len());
    for (i, plane) in planes.iter().enumerate() {
        let row_bytes = pixel_format::row_bytes_per_plane(format, frame_width, frame_height, i)?;
        let (tw, th) = pixel_format::plane_extent(format, frame_width, frame_height, i)?;
        let tex_fmt = pixel_format::plane_texture_format(format, i)?;
        let (padded, padded_stride) = padded_plane_staging(plane, row_bytes, th)?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cutlass_upload_plane"),
            size: wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: tex_fmt,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        upload_plane_texture(
            queue,
            &texture,
            &padded,
            padded_stride,
            tw,
            th,
        );
        textures.push(texture);
    }
    Ok(textures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use decoder::Plane;

    #[test]
    fn upload_handles_non_256_stride() {
        let row_bytes = 320u32;
        let height = 2u32;
        let stride = 320usize;
        let plane = Plane {
            stride,
            data: vec![0xabu8; stride * height as usize],
        };
        let (buf, ps) = padded_plane_staging(&plane, row_bytes, height).expect("staging");
        assert_eq!(ps, 512, "320-byte rows pad to 256-byte alignment");
        assert_eq!(buf.len(), (ps * height) as usize);
        assert_eq!(&buf[0..320], &vec![0xabu8; 320]);
        assert!(buf[320..512].iter().all(|&b| b == 0), "row padding zeroed");
        assert_eq!(&buf[512..512 + 320], &vec![0xabu8; 320]);
    }

    #[test]
    fn padded_plane_staging_rejects_buffer_shorter_than_row() {
        let plane = Plane {
            stride: 8,
            data: vec![0u8; 8],
        };
        assert!(matches!(
            padded_plane_staging(&plane, 16, 1),
            Err(RendererError::BadFrameLayout)
        ));
    }

    #[test]
    fn padded_plane_staging_allows_large_stride_small_row() {
        // Row logical width 4 bytes, but each source row is 64 bytes apart (simulates FFmpeg padding).
        let row_bytes = 4u32;
        let height = 2u32;
        let stride = 64usize;
        let mut data = vec![0u8; stride * height as usize];
        for row in 0..height as usize {
            data[row * stride] = (row + 1) as u8;
            data[row * stride + 1] = (row + 10) as u8;
            data[row * stride + 2] = (row + 20) as u8;
            data[row * stride + 3] = (row + 30) as u8;
        }
        let plane = Plane { stride, data };
        let (buf, ps) = padded_plane_staging(&plane, row_bytes, height).expect("staging");
        assert_eq!(ps, 256);
        assert_eq!(&buf[0..4], &[1, 10, 20, 30]);
        assert_eq!(&buf[256..260], &[2, 11, 21, 31]);
    }

    #[test]
    fn upload_cpu_frame_rejects_wrong_plane_count() {
        use decoder::PixelFormat;

        let dev_queue = pollster::block_on(async {
            let inst = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let ad = inst
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .expect("adapter");
            ad.request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: Default::default(),
                memory_hints: Default::default(),
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("device")
        });
        let (device, queue) = dev_queue;
        let planes = vec![
            Plane {
                data: vec![0u8; 4],
                stride: 2,
            },
            Plane {
                data: vec![0u8; 1],
                stride: 1,
            },
        ];
        assert!(matches!(
            upload_cpu_frame(
                &device,
                &queue,
                PixelFormat::Yuv420p,
                2,
                2,
                &planes
            ),
            Err(RendererError::BadFrameLayout)
        ));
    }

    #[test]
    fn align_up_matches_manual_formula_for_samples() {
        assert_eq!(super::align_up(320, 256), 512);
        assert_eq!(super::align_up(256, 256), 256);
        assert_eq!(super::align_up(1, 256), 256);
        assert_eq!(super::align_up(68, 256), 256);
    }
}
