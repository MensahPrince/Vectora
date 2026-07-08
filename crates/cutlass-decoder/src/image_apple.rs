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
    CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL, CGRect,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
};
use objc2_image_io::{
    CGImageSource, kCGImagePropertyAPNGDelayTime, kCGImagePropertyAPNGUnclampedDelayTime,
    kCGImagePropertyGIFDelayTime, kCGImagePropertyGIFDictionary,
    kCGImagePropertyGIFUnclampedDelayTime, kCGImagePropertyOrientation,
    kCGImagePropertyPNGDictionary, kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth,
    kCGImagePropertyWebPDelayTime, kCGImagePropertyWebPDictionary,
    kCGImagePropertyWebPUnclampedDelayTime, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceShouldCache,
    kCGImageSourceThumbnailMaxPixelSize,
};

use cutlass_core::{DecodeError, RgbaImage};

use crate::animation::{AnimationFrame, MAX_ANIMATION_DIMENSION, MAX_ANIMATION_FRAMES};
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
    decode_frame(
        &source,
        0,
        MAX_DECODE_DIMENSION,
        &path.display().to_string(),
    )
}

/// [`decode`] for in-memory encoded bytes (embedded assets).
pub fn decode_bytes(bytes: &[u8]) -> Result<RgbaImage, DecodeError> {
    let source = open_source_bytes(bytes)?;
    decode_frame(&source, 0, MAX_DECODE_DIMENSION, "embedded image")
}

/// Decode every frame of an animated image (GIF / APNG / animated WebP) from
/// in-memory bytes, with per-frame delays from the container's properties.
/// A static image yields one frame.
pub fn decode_animation_bytes(bytes: &[u8]) -> Result<Vec<AnimationFrame>, DecodeError> {
    let source = open_source_bytes(bytes)?;
    let count = unsafe { source.count() }.clamp(1, MAX_ANIMATION_FRAMES);
    let mut frames = Vec::with_capacity(count);
    for index in 0..count {
        let image = decode_frame(
            &source,
            index,
            MAX_ANIMATION_DIMENSION,
            &format!("embedded image frame {index}"),
        )?;
        frames.push(AnimationFrame {
            image,
            delay_ms: frame_delay_ms(&source, index),
        });
    }
    Ok(frames)
}

/// Decode one frame of `source` to straight-alpha RGBA8, oriented upright,
/// longest side capped at `max_dimension`.
fn decode_frame(
    source: &CGImageSource,
    index: usize,
    max_dimension: u32,
    ctx: &str,
) -> Result<RgbaImage, DecodeError> {
    let max = CFNumber::new_i64(i64::from(max_dimension));
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
    let image = unsafe { source.thumbnail_at_index(index, Some(options.as_opaque())) }
        .ok_or_else(|| DecodeError::Decode(format!("{ctx}: image decode failed")))?;

    let width = CGImage::width(Some(&image));
    let height = CGImage::height(Some(&image));
    if width == 0 || height == 0 {
        return Err(DecodeError::Decode(format!(
            "{ctx}: decoded image is empty"
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

/// Read frame `index`'s display delay in milliseconds from the container's
/// per-frame properties (GIF, APNG, or WebP dictionary; unclamped delay
/// preferred). Missing or zero delays fall back to the shared default.
fn frame_delay_ms(source: &CGImageSource, index: usize) -> u32 {
    let no: &CFType = CFBoolean::new(false).as_ref();
    let options = CFDictionary::<CFString, CFType>::from_slices(
        &[unsafe { kCGImageSourceShouldCache }],
        &[no],
    );
    let seconds = unsafe { source.properties_at_index(index, Some(options.as_opaque())) }
        .and_then(|props| {
            let containers: [(&CFString, [&CFString; 2]); 3] = unsafe {
                [
                    (
                        kCGImagePropertyGIFDictionary,
                        [
                            kCGImagePropertyGIFUnclampedDelayTime,
                            kCGImagePropertyGIFDelayTime,
                        ],
                    ),
                    (
                        kCGImagePropertyPNGDictionary,
                        [
                            kCGImagePropertyAPNGUnclampedDelayTime,
                            kCGImagePropertyAPNGDelayTime,
                        ],
                    ),
                    (
                        kCGImagePropertyWebPDictionary,
                        [
                            kCGImagePropertyWebPUnclampedDelayTime,
                            kCGImagePropertyWebPDelayTime,
                        ],
                    ),
                ]
            };
            containers.iter().find_map(|(container, keys)| {
                let dict = dict_dict(&props, container)?;
                keys.iter().find_map(|key| dict_f64(dict, key))
            })
        })
        .unwrap_or(0.0);
    crate::animation::normalize_delay_ms((seconds * 1000.0).round().max(0.0) as u32)
}

fn open_source(path: &Path) -> Result<CFRetained<CGImageSource>, DecodeError> {
    let url = CFURL::from_file_path(path)
        .ok_or_else(|| DecodeError::Open(format!("{}: bad path", path.display())))?;
    unsafe { CGImageSource::with_url(&url, None) }
        .ok_or_else(|| DecodeError::Open(format!("{}: unreadable image", path.display())))
}

fn open_source_bytes(bytes: &[u8]) -> Result<CFRetained<CGImageSource>, DecodeError> {
    let data = CFData::from_bytes(bytes);
    unsafe { CGImageSource::with_data(&data, None) }
        .ok_or_else(|| DecodeError::Open("embedded image: unreadable bytes".into()))
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

/// Read a float property from an untyped `CFDictionary`.
fn dict_f64(dict: &CFDictionary, key: &CFString) -> Option<f64> {
    let value = unsafe { dict.value(key as *const CFString as *const c_void) };
    if value.is_null() {
        return None;
    }
    // SAFETY: ImageIO documents delay values as CFNumber; read-only access.
    let number = unsafe { &*value.cast::<CFNumber>() };
    number.as_f64()
}

/// Read a nested dictionary property from an untyped `CFDictionary`.
fn dict_dict<'a>(dict: &'a CFDictionary, key: &CFString) -> Option<&'a CFDictionary> {
    let value = unsafe { dict.value(key as *const CFString as *const c_void) };
    if value.is_null() {
        return None;
    }
    // SAFETY: ImageIO documents per-container frame properties (GIF/PNG/WebP
    // keys) as CFDictionary; the parent retains it and we only read.
    Some(unsafe { &*value.cast::<CFDictionary>() })
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
