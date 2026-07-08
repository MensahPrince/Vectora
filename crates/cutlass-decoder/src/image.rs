//! Still-image probe and decode.
//!
//! Photos are half of every mobile media picker, so stills get the same
//! FFmpeg-free treatment as video: sniff the container by magic bytes (never
//! by file extension), read dimensions without decoding for the media pool,
//! and decode the single frame to straight-alpha RGBA when the renderer
//! actually composites it.
//!
//! Backends mirror the video decoder split:
//!
//! - **Apple** ([`crate::image_apple`]): ImageIO / `CGImageSource`, which
//!   covers HEIC/HEIF (the iPhone camera default), WebP, GIF, TIFF, and BMP
//!   on top of PNG/JPEG, and applies EXIF orientation.
//! - **Portable** ([`portable`]): pure-Rust PNG + JPEG, compiled everywhere.
//!   It is the primary path on platforms with no native image backend yet
//!   and the fallback behind ImageIO on Apple.

use std::io::Read;
use std::path::Path;

use cutlass_core::{DecodeError, RgbaImage};

/// Image containers recognized by [`sniff_image`]'s magic-byte check.
///
/// Recognition is deliberately broader than what every backend can decode:
/// classifying a file as a still is what routes it away from the video
/// probe, and an unsupported-format error from the image path is more
/// truthful than a demuxer error from the video path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    WebP,
    /// HEIF family (HEIC/AVIF): ISO-BMFF `ftyp` with an image major brand.
    Heif,
    Gif,
    Bmp,
    Tiff,
}

/// Static metadata for a still image, read without decoding pixels.
///
/// Dimensions are *display* dimensions: the Apple backend swaps
/// width/height for EXIF orientations that rotate by 90°, matching what
/// [`decode_image`] produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
}

/// Bytes to read for [`sniff_image`]: enough for every magic we check
/// (the longest probe is the 12-byte ISO-BMFF `ftyp` + brand window).
const SNIFF_LEN: u64 = 16;

/// Classify `path` as a still image by magic bytes, or `None` if the header
/// matches no known image container (including unreadable/short files).
pub fn sniff_image(path: &Path) -> Option<ImageFormat> {
    let file = std::fs::File::open(path).ok()?;
    let mut head = Vec::with_capacity(SNIFF_LEN as usize);
    file.take(SNIFF_LEN).read_to_end(&mut head).ok()?;
    sniff_bytes(&head)
}

/// [`sniff_image`] on an in-memory header (at least [`SNIFF_LEN`] bytes of
/// the file, shorter is tolerated).
pub fn sniff_bytes(head: &[u8]) -> Option<ImageFormat> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(ImageFormat::Png);
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageFormat::Jpeg);
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some(ImageFormat::WebP);
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        return Some(ImageFormat::Tiff);
    }
    if head.starts_with(b"BM") {
        return Some(ImageFormat::Bmp);
    }
    // ISO-BMFF: video containers (mp4/mov) share the `ftyp` box, so only an
    // image major brand classifies as a still.
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        const IMAGE_BRANDS: &[&[u8; 4]] = &[
            b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1", b"avif",
            b"avis",
        ];
        let brand: &[u8] = &head[8..12];
        if IMAGE_BRANDS.iter().any(|b| &b[..] == brand) {
            return Some(ImageFormat::Heif);
        }
    }
    None
}

/// Read `path`'s display dimensions without decoding pixels.
///
/// Errors with [`DecodeError::Open`] when the file isn't a recognized image,
/// so callers can fall through to other probes.
pub fn probe_image(path: &Path) -> Result<ImageInfo, DecodeError> {
    let format = require_image(path)?;
    let info = backend_probe(path, format)?;
    if info.width == 0 || info.height == 0 {
        return Err(DecodeError::Decode(format!(
            "{}: image reports zero dimensions",
            path.display()
        )));
    }
    Ok(info)
}

/// Decode `path`'s single frame to straight-alpha RGBA8.
///
/// The Apple backend applies EXIF orientation and caps the longest side at
/// [`MAX_DECODE_DIMENSION`] (aspect preserved) so one 48-megapixel photo
/// cannot blow the mobile memory budget; the composited quad is sized from
/// the probe's full dimensions, so a capped bitmap only changes sampling
/// density, never layout.
pub fn decode_image(path: &Path) -> Result<RgbaImage, DecodeError> {
    let format = require_image(path)?;
    let image = backend_decode(path, format)?;
    if image.width == 0 || image.height == 0 {
        return Err(DecodeError::Decode(format!(
            "{}: image decoded to zero pixels",
            path.display()
        )));
    }
    Ok(image)
}

/// [`decode_image`] for an in-memory encoded image (embedded assets such as
/// bundled stickers). Same output contract: straight-alpha RGBA8, oriented
/// upright, longest side capped at [`MAX_DECODE_DIMENSION`].
pub fn decode_image_bytes(bytes: &[u8]) -> Result<RgbaImage, DecodeError> {
    let format = sniff_bytes(bytes).ok_or_else(|| {
        DecodeError::Open("embedded image: not a recognized image container".into())
    })?;
    let image = backend_decode_bytes(bytes, format)?;
    if image.width == 0 || image.height == 0 {
        return Err(DecodeError::Decode(
            "embedded image: decoded to zero pixels".into(),
        ));
    }
    Ok(image)
}

/// Upper bound on a decoded still's longest side (Apple backend). 4096 px
/// keeps a worst-case bitmap at ~50 MB RGBA while comfortably out-resolving
/// a 4K canvas.
pub const MAX_DECODE_DIMENSION: u32 = 4096;

fn require_image(path: &Path) -> Result<ImageFormat, DecodeError> {
    sniff_image(path).ok_or_else(|| {
        DecodeError::Open(format!("{}: not a recognized still image", path.display()))
    })
}

#[cfg(target_vendor = "apple")]
fn backend_probe(path: &Path, format: ImageFormat) -> Result<ImageInfo, DecodeError> {
    // ImageIO first; the pure-Rust path only rescues formats it knows
    // (surfacing the ImageIO error otherwise, which names the real failure).
    crate::image_apple::probe(path)
        .or_else(|apple_err| portable::probe(path, format).map_err(|_| apple_err))
}

#[cfg(not(target_vendor = "apple"))]
fn backend_probe(path: &Path, format: ImageFormat) -> Result<ImageInfo, DecodeError> {
    portable::probe(path, format)
}

#[cfg(target_vendor = "apple")]
fn backend_decode(path: &Path, format: ImageFormat) -> Result<RgbaImage, DecodeError> {
    crate::image_apple::decode(path)
        .or_else(|apple_err| portable::decode(path, format).map_err(|_| apple_err))
}

#[cfg(not(target_vendor = "apple"))]
fn backend_decode(path: &Path, format: ImageFormat) -> Result<RgbaImage, DecodeError> {
    portable::decode(path, format)
}

#[cfg(target_vendor = "apple")]
fn backend_decode_bytes(bytes: &[u8], format: ImageFormat) -> Result<RgbaImage, DecodeError> {
    crate::image_apple::decode_bytes(bytes)
        .or_else(|apple_err| portable::decode_bytes(bytes, format).map_err(|_| apple_err))
}

#[cfg(not(target_vendor = "apple"))]
fn backend_decode_bytes(bytes: &[u8], format: ImageFormat) -> Result<RgbaImage, DecodeError> {
    portable::decode_bytes(bytes, format)
}

/// Pure-Rust PNG/JPEG backend, compiled on every platform.
pub mod portable {
    use std::path::Path;

    use cutlass_core::{DecodeError, RgbaImage};

    use super::{ImageFormat, ImageInfo};

    pub fn probe(path: &Path, format: ImageFormat) -> Result<ImageInfo, DecodeError> {
        match format {
            ImageFormat::Png => png_probe(path),
            ImageFormat::Jpeg => jpeg_probe(path),
            other => Err(unsupported(&path.display().to_string(), other)),
        }
    }

    pub fn decode(path: &Path, format: ImageFormat) -> Result<RgbaImage, DecodeError> {
        match format {
            ImageFormat::Png => png_decode(path),
            ImageFormat::Jpeg => jpeg_decode(path),
            other => Err(unsupported(&path.display().to_string(), other)),
        }
    }

    /// [`decode`] for in-memory encoded bytes (embedded assets).
    pub fn decode_bytes(bytes: &[u8], format: ImageFormat) -> Result<RgbaImage, DecodeError> {
        const CTX: &str = "embedded image";
        match format {
            ImageFormat::Png => png_decode_reader(std::io::Cursor::new(bytes), CTX),
            ImageFormat::Jpeg => jpeg_decode_bytes(bytes.to_vec(), CTX),
            other => Err(unsupported(CTX, other)),
        }
    }

    fn unsupported(ctx: &str, format: ImageFormat) -> DecodeError {
        DecodeError::unsupported(format!(
            "{ctx}: no portable decoder for {format:?} stills on this platform"
        ))
    }

    fn open(path: &Path) -> Result<std::io::BufReader<std::fs::File>, DecodeError> {
        let file = std::fs::File::open(path)
            .map_err(|e| DecodeError::Io(format!("{}: {e}", path.display())))?;
        Ok(std::io::BufReader::new(file))
    }

    fn png_probe(path: &Path) -> Result<ImageInfo, DecodeError> {
        let reader = png::Decoder::new(open(path)?)
            .read_info()
            .map_err(|e| DecodeError::Open(format!("{}: {e}", path.display())))?;
        let info = reader.info();
        Ok(ImageInfo {
            width: info.width,
            height: info.height,
        })
    }

    fn png_decode(path: &Path) -> Result<RgbaImage, DecodeError> {
        png_decode_reader(open(path)?, &path.display().to_string())
    }

    fn png_decode_reader(reader: impl std::io::Read, ctx: &str) -> Result<RgbaImage, DecodeError> {
        let mut decoder = png::Decoder::new(reader);
        // Palette/gray expansion + 16→8 bit strip: everything below is 8-bit.
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder
            .read_info()
            .map_err(|e| DecodeError::Open(format!("{ctx}: {e}")))?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let frame = reader
            .next_frame(&mut buf)
            .map_err(|e| DecodeError::Decode(format!("{ctx}: {e}")))?;
        buf.truncate(frame.buffer_size());
        let (width, height) = (frame.width, frame.height);
        let pixels = match frame.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => expand(&buf, 3, |px| [px[0], px[1], px[2], 255]),
            png::ColorType::Grayscale => expand(&buf, 1, |px| [px[0], px[0], px[0], 255]),
            png::ColorType::GrayscaleAlpha => expand(&buf, 2, |px| [px[0], px[0], px[0], px[1]]),
            png::ColorType::Indexed => {
                // normalize_to_color8 expands palettes; reaching here is a bug.
                return Err(DecodeError::Decode(format!("{ctx}: palette not expanded")));
            }
        };
        Ok(RgbaImage::new(width, height, pixels))
    }

    fn jpeg_probe(path: &Path) -> Result<ImageInfo, DecodeError> {
        let bytes =
            std::fs::read(path).map_err(|e| DecodeError::Io(format!("{}: {e}", path.display())))?;
        let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(bytes));
        decoder
            .decode_headers()
            .map_err(|e| DecodeError::Open(format!("{}: {e:?}", path.display())))?;
        let (width, height) = decoder
            .dimensions()
            .ok_or_else(|| DecodeError::Open(format!("{}: no dimensions", path.display())))?;
        Ok(ImageInfo {
            width: width as u32,
            height: height as u32,
        })
    }

    fn jpeg_decode(path: &Path) -> Result<RgbaImage, DecodeError> {
        let bytes =
            std::fs::read(path).map_err(|e| DecodeError::Io(format!("{}: {e}", path.display())))?;
        jpeg_decode_bytes(bytes, &path.display().to_string())
    }

    fn jpeg_decode_bytes(bytes: Vec<u8>, ctx: &str) -> Result<RgbaImage, DecodeError> {
        use zune_jpeg::zune_core::colorspace::ColorSpace;
        use zune_jpeg::zune_core::options::DecoderOptions;

        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder =
            zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(bytes), options);
        let data = decoder
            .decode()
            .map_err(|e| DecodeError::Decode(format!("{ctx}: {e:?}")))?;
        let (width, height) = decoder
            .dimensions()
            .ok_or_else(|| DecodeError::Decode(format!("{ctx}: no dimensions")))?;
        // The decoder may override the requested colorspace (e.g. CMYK
        // sources); normalize whatever it actually produced.
        let pixels = match decoder.output_colorspace() {
            Some(ColorSpace::RGBA) => data,
            Some(ColorSpace::RGB) => expand(&data, 3, |px| [px[0], px[1], px[2], 255]),
            Some(ColorSpace::Luma) => expand(&data, 1, |px| [px[0], px[0], px[0], 255]),
            Some(ColorSpace::LumaA) => expand(&data, 2, |px| [px[0], px[0], px[0], px[1]]),
            other => {
                return Err(DecodeError::unsupported(format!(
                    "{ctx}: jpeg decoded to unsupported colorspace {other:?}"
                )));
            }
        };
        let expected = width * height * 4;
        if pixels.len() != expected {
            return Err(DecodeError::Decode(format!(
                "{ctx}: decoded {} bytes, expected {expected}",
                pixels.len()
            )));
        }
        Ok(RgbaImage::new(width as u32, height as u32, pixels))
    }

    /// Expand `channels`-per-pixel rows to RGBA with `map`.
    fn expand(data: &[u8], channels: usize, map: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() / channels * 4);
        for px in data.chunks_exact(channels) {
            out.extend_from_slice(&map(px));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_png_jpeg_webp_magic() {
        assert_eq!(
            sniff_bytes(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            sniff_bytes(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F']),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            sniff_bytes(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some(ImageFormat::WebP)
        );
        assert_eq!(sniff_bytes(b"GIF89a\x01\x00"), Some(ImageFormat::Gif));
        assert_eq!(sniff_bytes(b"II*\0\x08\0\0\0"), Some(ImageFormat::Tiff));
        assert_eq!(sniff_bytes(b"BM\x36\x00\x00\x00"), Some(ImageFormat::Bmp));
    }

    #[test]
    fn sniffs_heif_brands_but_not_video_ftyp() {
        assert_eq!(
            sniff_bytes(b"\0\0\0\x18ftypheic\0\0\0\0"),
            Some(ImageFormat::Heif)
        );
        assert_eq!(
            sniff_bytes(b"\0\0\0\x18ftypmif1\0\0\0\0"),
            Some(ImageFormat::Heif)
        );
        assert_eq!(
            sniff_bytes(b"\0\0\0\x18ftypavif\0\0\0\0"),
            Some(ImageFormat::Heif)
        );
        // mp4/mov major brands stay on the video path.
        assert_eq!(sniff_bytes(b"\0\0\0\x18ftypisom\0\0\0\0"), None);
        assert_eq!(sniff_bytes(b"\0\0\0\x18ftypmp42\0\0\0\0"), None);
        assert_eq!(sniff_bytes(b"\0\0\0\x14ftypqt  \0\0\0\0"), None);
    }

    #[test]
    fn short_or_unknown_headers_are_not_images() {
        assert_eq!(sniff_bytes(b""), None);
        assert_eq!(sniff_bytes(b"\x89PN"), None);
        assert_eq!(sniff_bytes(b"RIFF\x24\x00\x00\x00WAVE"), None);
        assert_eq!(sniff_bytes(b"plain text file"), None);
    }
}
