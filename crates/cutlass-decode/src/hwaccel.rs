//! Hardware decode via FFmpeg `AVHWDeviceContext` (same model as [ff-decode](https://docs.rs/ff-decode)).
//!
//! Decoded GPU surfaces are transferred to CPU memory with `av_hwframe_transfer_data`
//! before returning [`DecodedFrame`](crate::DecodedFrame). A future `FrameData::Gpu` variant
//! can skip this copy when the compositor accepts platform surfaces.

use std::ptr;

use ffmpeg_next::codec::context::Context;
use ffmpeg_next::ffi::{
    self, AVBufferRef, AVCodecContext, AVHWDeviceType, AVPixelFormat, av_buffer_ref,
    av_buffer_unref, av_hwdevice_ctx_create, av_hwframe_transfer_data,
};
use ffmpeg_next::util::frame::video::Video;
use tracing::{debug, info, warn};

use crate::error::DecodeError;

/// Hardware acceleration policy (mirrors ff-decode `HardwareAccel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwAccel {
    /// Probe platform backends in priority order; fall back to software.
    #[default]
    Auto,
    /// CPU decode only.
    None,
    /// Apple VideoToolbox (macOS / iOS).
    VideoToolbox,
    /// VA-API (Linux).
    #[cfg(any(target_os = "linux", doc))]
    Vaapi,
    /// NVIDIA NVDEC (`CUDA` device).
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        target_os = "windows",
        doc
    ))]
    Nvdec,
    /// Intel Quick Sync (`QSV` device).
    #[cfg(any(target_os = "linux", target_os = "windows", doc))]
    Qsv,
    /// Direct3D 11 (Windows).
    #[cfg(any(target_os = "windows", doc))]
    D3d11va,
}

impl HwAccel {
    pub const fn name(self) -> &'static str {
        match self {
            HwAccel::Auto => "auto",
            HwAccel::None => "none",
            HwAccel::VideoToolbox => "videotoolbox",
            #[cfg(any(target_os = "linux", doc))]
            HwAccel::Vaapi => "vaapi",
            #[cfg(any(
                all(target_os = "linux", target_arch = "x86_64"),
                target_os = "windows",
                doc
            ))]
            HwAccel::Nvdec => "nvdec",
            #[cfg(any(target_os = "linux", target_os = "windows", doc))]
            HwAccel::Qsv => "qsv",
            #[cfg(any(target_os = "windows", doc))]
            HwAccel::D3d11va => "d3d11va",
        }
    }

    pub const fn uses_hardware(self) -> bool {
        !matches!(self, HwAccel::Auto | HwAccel::None)
    }

    fn device_type(self) -> Option<AVHWDeviceType> {
        match self {
            HwAccel::Auto | HwAccel::None => None,
            HwAccel::VideoToolbox => Some(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX),
            #[cfg(any(target_os = "linux", doc))]
            HwAccel::Vaapi => Some(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI),
            #[cfg(any(
                all(target_os = "linux", target_arch = "x86_64"),
                target_os = "windows",
                doc
            ))]
            HwAccel::Nvdec => Some(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA),
            #[cfg(any(target_os = "linux", target_os = "windows", doc))]
            HwAccel::Qsv => Some(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_QSV),
            #[cfg(any(target_os = "windows", doc))]
            HwAccel::D3d11va => Some(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA),
        }
    }
}

/// Per-session decode options.
#[derive(Debug, Clone, Copy)]
pub struct DecodeOptions {
    pub hw_accel: HwAccel,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            hw_accel: HwAccel::Auto,
        }
    }
}

impl DecodeOptions {
    pub fn hw_accel(mut self, accel: HwAccel) -> Self {
        self.hw_accel = accel;
        self
    }
}

const fn auto_probe_order() -> &'static [HwAccel] {
    #[cfg(target_os = "macos")]
    {
        &[HwAccel::VideoToolbox]
    }
    #[cfg(target_os = "linux")]
    {
        &[HwAccel::Vaapi, HwAccel::Nvdec, HwAccel::Qsv]
    }
    #[cfg(target_os = "windows")]
    {
        &[HwAccel::D3d11va, HwAccel::Nvdec, HwAccel::Qsv]
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        &[]
    }
}

const fn hw_pixel_format_priority() -> &'static [AVPixelFormat] {
    #[cfg(target_os = "macos")]
    {
        &[ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX]
    }
    #[cfg(target_os = "linux")]
    {
        &[
            ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
            ffi::AVPixelFormat::AV_PIX_FMT_CUDA,
            ffi::AVPixelFormat::AV_PIX_FMT_QSV,
        ]
    }
    #[cfg(target_os = "windows")]
    {
        &[
            ffi::AVPixelFormat::AV_PIX_FMT_D3D11,
            ffi::AVPixelFormat::AV_PIX_FMT_CUDA,
            ffi::AVPixelFormat::AV_PIX_FMT_QSV,
        ]
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        &[]
    }
}

unsafe extern "C" fn select_hw_pixel_format(
    _ctx: *mut AVCodecContext,
    pix_fmts: *const AVPixelFormat,
) -> AVPixelFormat {
    unsafe {
        for &preferred in hw_pixel_format_priority() {
            let mut p = pix_fmts;
            while *p != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
                if *p == preferred {
                    return preferred;
                }
                p = p.add(1);
            }
        }
        *pix_fmts
    }
}

/// Attach a hardware device to `ctx` when requested. Returns the active mode (may be `None`).
pub fn attach(ctx: &mut Context, requested: HwAccel) -> Result<HwAccel, DecodeError> {
    match requested {
        HwAccel::None => Ok(HwAccel::None),
        HwAccel::Auto => {
            for &candidate in auto_probe_order() {
                match try_attach(ctx, candidate) {
                    Ok(()) => {
                        info!(backend = candidate.name(), "hwaccel enabled");
                        return Ok(candidate);
                    }
                    Err(e) => {
                        debug!(
                            backend = candidate.name(),
                            error = %e,
                            "hwaccel probe failed"
                        );
                    }
                }
            }
            info!("hwaccel unavailable, using software decode");
            Ok(HwAccel::None)
        }
        specific => try_attach(ctx, specific).map(|()| {
            info!(backend = specific.name(), "hwaccel enabled");
            specific
        }),
    }
}

fn try_attach(ctx: &mut Context, accel: HwAccel) -> Result<(), DecodeError> {
    let Some(device_type) = accel.device_type() else {
        return Err(DecodeError::HwAccelUnavailable {
            accel: accel.name(),
        });
    };

    unsafe {
        let mut hw_device_ctx: *mut AVBufferRef = ptr::null_mut();
        let ret = av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            device_type,
            ptr::null(),
            ptr::null_mut(),
            0,
        );
        if ret < 0 {
            return Err(DecodeError::HwAccelUnavailable {
                accel: accel.name(),
            });
        }

        let raw = ctx.as_mut_ptr();
        (*raw).hw_device_ctx = hw_device_ctx;
        (*raw).get_format = Some(select_hw_pixel_format);
        (*raw).extra_hw_frames = 8;
    }

    Ok(())
}

pub fn is_hardware_pixel_format(format: ffmpeg_next::util::format::pixel::Pixel) -> bool {
    use ffmpeg_next::util::format::pixel::Pixel;
    matches!(
        format,
        Pixel::VIDEOTOOLBOX
            | Pixel::VAAPI
            | Pixel::CUDA
            | Pixel::D3D11
            | Pixel::DXVA2_VLD
            | Pixel::QSV
            | Pixel::VDPAU
            | Pixel::MEDIACODEC
            | Pixel::VULKAN
    )
}

/// Copy a hardware frame into `sw` (CPU) memory.
pub fn transfer_to_cpu(hw: &Video, sw: &mut Video) -> Result<(), DecodeError> {
    unsafe {
        let ret = av_hwframe_transfer_data(sw.as_mut_ptr(), hw.as_ptr(), 0);
        if ret < 0 {
            return Err(DecodeError::Decode(ffmpeg_next::Error::from(ret)));
        }
        let sw_ptr = sw.as_mut_ptr();
        let hw_ptr = hw.as_ptr();
        (*sw_ptr).pts = (*hw_ptr).pts;
        (*sw_ptr).pkt_dts = (*hw_ptr).pkt_dts;
        (*sw_ptr).best_effort_timestamp = (*hw_ptr).best_effort_timestamp;
    }
    Ok(())
}

/// Retain an extra ref so the device outlives the codec context if needed.
pub fn retain_device_ref(ctx: &Context) -> Option<*mut AVBufferRef> {
    unsafe {
        let device = (*ctx.as_ptr()).hw_device_ctx;
        if device.is_null() {
            return None;
        }
        let retained = av_buffer_ref(device);
        if retained.is_null() {
            warn!("av_buffer_ref on hw_device_ctx returned null");
            None
        } else {
            Some(retained)
        }
    }
}

pub fn release_device_ref(device: &mut Option<*mut AVBufferRef>) {
    if let Some(ptr) = device.take() {
        unsafe {
            av_buffer_unref(&mut (ptr as *mut _));
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn auto_priority_includes_videotoolbox_on_macos() {
        assert!(auto_probe_order().contains(&HwAccel::VideoToolbox));
    }
}
