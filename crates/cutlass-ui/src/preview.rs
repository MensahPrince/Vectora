//! Preview frame helpers: engine RGBA buffers → Slint images.

use cutlass_engine::RgbaFrame;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

pub fn to_slint_image(frame: RgbaFrame) -> Image {
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&frame.bytes, frame.width, frame.height);
    Image::from_rgba8(buffer)
}
