//! Per-lane clip colors for the Slint timeline (not stored on engine tracks).

use cutlass_models::TrackKind;

const VIDEO: &[(u8, u8, u8)] = &[
    (0x4A, 0x6F, 0xA5),
    (0x5E, 0x8B, 0x7E),
    (0x6C, 0x5B, 0x7B),
    (0x54, 0x7A, 0x8F),
    (0x7D, 0x8A, 0xA8),
];

const AUDIO: &[(u8, u8, u8)] = &[
    (0xC9, 0x98, 0x46),
    (0xBF, 0x6F, 0x4A),
    (0xA6, 0x6B, 0x5F),
    (0xC7, 0x7F, 0x4D),
];

pub fn track_color(kind: TrackKind, kind_index: usize) -> slint::Color {
    let palette = match kind {
        TrackKind::Video => VIDEO,
        TrackKind::Audio => AUDIO,
    };
    let (r, g, b) = palette[kind_index % palette.len()];
    slint::Color::from_rgb_u8(r, g, b)
}
