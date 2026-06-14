//! Still-image decode (PNG/JPEG/WebP) for image media.
//!
//! FFmpeg demuxes stills as single-frame video streams (`png_pipe`,
//! `jpeg_pipe`, `webp_pipe`, …), so this is the poster-frame path of
//! [`video_thumbnail`](crate::video::video_thumbnail) minus the seek:
//! open, decode the one frame, convert to tightly packed RGBA. Alpha
//! survives the conversion, so transparent PNGs composite correctly.
//!
//! Decode is a cold path — the engine caches the RGBA result per media and
//! reuses it for every frame the still is visible (a still never changes).

use std::path::Path;

use ffmpeg_next::codec;
use ffmpeg_next::format;
use ffmpeg_next::media::Type;

use crate::error::DecodeError;
use crate::video::ThumbnailImage;
use crate::video::ensure_ffmpeg_init;
use crate::video::{decode_first_frame, scale_to_rgba};

/// Cap on decoded still dimensions: bounds memory (a 4096² RGBA still is
/// 64 MiB) and stays within every real GPU's texture limits while exceeding
/// the 4K canvas — the compositor scales the upload into place.
pub const STILL_MAX_DIM: u32 = 4096;

/// Decode a still image to tightly packed RGBA, downscaled to fit within
/// `max_w` × `max_h` (aspect preserved, never upscaled).
pub fn decode_image(path: &Path, max_w: u32, max_h: u32) -> Result<ThumbnailImage, DecodeError> {
    ensure_ffmpeg_init()?;
    if max_w == 0 || max_h == 0 {
        return Err(DecodeError::unsupported("zero image dimensions requested"));
    }

    let path_str = path
        .to_str()
        .ok_or_else(|| DecodeError::unsupported("path is not valid UTF-8"))?;
    let mut input = format::input(path_str).map_err(DecodeError::Open)?;

    let stream = input
        .streams()
        .best(Type::Video)
        .ok_or_else(|| DecodeError::unsupported("no decodable picture in image file"))?;
    let stream_index = stream.index();

    let mut decoder = codec::Context::from_parameters(stream.parameters())
        .map_err(DecodeError::Open)?
        .decoder()
        .video()
        .map_err(DecodeError::Open)?;

    let frame = decode_first_frame(&mut input, &mut decoder, stream_index)?;
    scale_to_rgba(&frame, max_w, max_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn png_asset() -> Option<PathBuf> {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local-assets/assets/texture.png");
        path.exists().then_some(path)
    }

    #[test]
    fn decodes_png_to_packed_rgba() {
        let Some(path) = png_asset() else {
            return;
        };
        let img = decode_image(&path, STILL_MAX_DIM, STILL_MAX_DIM).expect("decode png");
        assert!(img.width > 0 && img.height > 0);
        assert!(img.width <= STILL_MAX_DIM && img.height <= STILL_MAX_DIM);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
    }

    #[test]
    fn downscales_into_box_preserving_aspect() {
        let Some(path) = png_asset() else {
            return;
        };
        let native = decode_image(&path, STILL_MAX_DIM, STILL_MAX_DIM).expect("native");
        let boxed = decode_image(&path, 64, 64).expect("boxed");
        assert!(boxed.width <= 64 && boxed.height <= 64);
        // Aspect preserved within rounding.
        let native_aspect = f64::from(native.width) / f64::from(native.height);
        let boxed_aspect = f64::from(boxed.width) / f64::from(boxed.height);
        assert!((native_aspect - boxed_aspect).abs() < 0.05);
    }

    #[test]
    fn zero_box_is_rejected() {
        let Some(path) = png_asset() else {
            return;
        };
        assert!(matches!(
            decode_image(&path, 0, 64),
            Err(DecodeError::Unsupported { .. })
        ));
    }

    #[test]
    fn non_image_garbage_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-an-image.png");
        std::fs::write(&path, b"definitely not a png").unwrap();
        assert!(decode_image(&path, 64, 64).is_err());
    }
}
