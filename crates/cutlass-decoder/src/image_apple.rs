//! Apple still-image backend: ImageIO (`CGImageSource`) + CoreGraphics.
//!
//! ImageIO is the system image codec surface (what Photos itself uses), so
//! this path covers HEIC/HEIF — the iPhone camera default — plus WebP, GIF,
//! TIFF, and BMP on top of PNG/JPEG, with EXIF orientation applied.
//!
//! Decode strategy: `CGImageSourceCreateThumbnailAtIndex` with
//! `kCGImageSourceCreateThumbnailWithTransform`, which bakes the orientation
//! transform into the pixels and caps the longest side (see
//! [`MAX_DECODE_DIMENSION`]) in one shot. The resulting `CGImage` is then
//! normalized by drawing into an RGBA8 `CGBitmapContext`. CoreGraphics only
//! draws premultiplied, and the compositor premultiplies on upload, so the
//! buffer is un-premultiplied before returning (a no-op for opaque photos).

use std::ffi::c_void;
use std::path::Path;

use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL, CGRect,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
};
use objc2_image_io::{
    CGImageSource, kCGImagePropertyOrientation, kCGImagePropertyPixelHeight,
    kCGImagePropertyPixelWidth, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceShouldCache,
    kCGImageSourceThumbnailMaxPixelSize,
};

use cutlass_core::{DecodeError, RgbaImage};

use crate::image::{ImageInfo, MAX_DECODE_DIMENSION};

/// Read display dimensions from the image's properties (no pixel decode).
/// EXIF orientations 5–8 rotate by 90°, so width/height swap.
pub fn probe(path: &Path) -> Result<ImageInfo, DecodeError> {
    let source = open_source(path)?;
    // Metadata read only — don't leave a decoded frame in ImageIO's cache.
    let no: &CFType = CFBoolean::new(false).as_ref();
    let options = CFDictionary::<CFString, CFType>::from_slices(
        &[unsafe { kCGImageSourceShouldCache }],
        &[no],
    );
    let props = unsafe { source.properties_at_index(0, Some(options.as_opaque())) }
        .ok_or_else(|| DecodeError::Open(format!("{}: no image properties", path.display())))?;
    let width = dict_i64(&props, unsafe { kCGImagePropertyPixelWidth })
        .ok_or_else(|| DecodeError::Open(format!("{}: no pixel width", path.display())))?;
    let height = dict_i64(&props, unsafe { kCGImagePropertyPixelHeight })
        .ok_or_else(|| DecodeError::Open(format!("{}: no pixel height", path.display())))?;
    let orientation = dict_i64(&props, unsafe { kCGImagePropertyOrientation }).unwrap_or(1);

    let (width, height) = (clamp_dim(width), clamp_dim(height));
    if orientation >= 5 {
        Ok(ImageInfo {
            width: height,
            height: width,
        })
    } else {
        Ok(ImageInfo { width, height })
    }
}

/// Decode the first frame to straight-alpha RGBA8, oriented upright, longest
/// side capped at [`MAX_DECODE_DIMENSION`].
pub fn decode(path: &Path) -> Result<RgbaImage, DecodeError> {
    let source = open_source(path)?;
    let max = CFNumber::new_i64(i64::from(MAX_DECODE_DIMENSION));
    let max: &CFType = max.as_ref();
    let yes: &CFType = CFBoolean::new(true).as_ref();
    let options = CFDictionary::<CFString, CFType>::from_slices(
        &[
            unsafe { kCGImageSourceCreateThumbnailWithTransform },
            unsafe { kCGImageSourceCreateThumbnailFromImageAlways },
            unsafe { kCGImageSourceThumbnailMaxPixelSize },
        ],
        &[yes, yes, max],
    );
    let image = unsafe { source.thumbnail_at_index(0, Some(options.as_opaque())) }
        .ok_or_else(|| DecodeError::Decode(format!("{}: image decode failed", path.display())))?;

    let width = CGImage::width(Some(&image));
    let height = CGImage::height(Some(&image));
    if width == 0 || height == 0 {
        return Err(DecodeError::Decode(format!(
            "{}: decoded image is empty",
            path.display()
        )));
    }

    // Normalize any source format (CMYK, 16-bit, indexed, ...) by drawing
    // into an sRGB RGBA8 bitmap. CG only renders premultiplied alpha.
    let mut pixels = vec![0u8; width * height * 4];
    let space = CGColorSpace::new_device_rgb()
        .ok_or_else(|| DecodeError::Decode("no RGB color space".into()))?;
    let ctx = unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr().cast::<c_void>(),
            width,
            height,
            8,
            width * 4,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    }
    .ok_or_else(|| DecodeError::Decode("bitmap context creation failed".into()))?;
    let rect = CGRect::new(
        objc2_core_foundation::CGPoint::new(0.0, 0.0),
        objc2_core_foundation::CGSize::new(width as f64, height as f64),
    );
    CGContext::draw_image(Some(&ctx), rect, Some(&image));
    CGContext::flush(Some(&ctx));
    drop(ctx);

    unpremultiply(&mut pixels);
    Ok(RgbaImage::new(width as u32, height as u32, pixels))
}

fn open_source(path: &Path) -> Result<CFRetained<CGImageSource>, DecodeError> {
    let url = CFURL::from_file_path(path)
        .ok_or_else(|| DecodeError::Open(format!("{}: bad path", path.display())))?;
    unsafe { CGImageSource::with_url(&url, None) }
        .ok_or_else(|| DecodeError::Open(format!("{}: unreadable image", path.display())))
}

/// Read a numeric property from an untyped `CFDictionary`.
fn dict_i64(dict: &CFDictionary, key: &CFString) -> Option<i64> {
    let value = unsafe { dict.value(key as *const CFString as *const c_void) };
    if value.is_null() {
        return None;
    }
    // SAFETY: ImageIO documents these property values as CFNumber; the
    // dictionary retains the value for its own lifetime and we only read.
    let number = unsafe { &*value.cast::<CFNumber>() };
    number.as_i64()
}

fn clamp_dim(v: i64) -> u32 {
    u32::try_from(v.max(0)).unwrap_or(u32::MAX)
}

/// Convert premultiplied RGBA (CG's only draw format) back to the straight
/// alpha the compositor expects on upload. Opaque pixels pass through.
fn unpremultiply(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 || a == 255 {
            continue;
        }
        let a16 = u16::from(a);
        for c in &mut px[..3] {
            // Round-to-nearest un-premultiply: c * 255 / a.
            *c = ((u16::from(*c) * 255 + a16 / 2) / a16).min(255) as u8;
        }
    }
}
