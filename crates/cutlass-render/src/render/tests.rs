use super::StickerSequence;
use cutlass_core::RgbaImage;

fn seq(delays_ms: &[u32]) -> StickerSequence {
    StickerSequence {
        frames: delays_ms
            .iter()
            .map(|_| RgbaImage::new(1, 1, vec![0; 4]))
            .collect(),
        delays_ms: delays_ms.to_vec(),
        total_ms: delays_ms.iter().map(|d| u64::from(*d)).sum(),
    }
}

#[test]
fn sticker_frame_selection_walks_delays_and_loops() {
    let s = seq(&[100, 50, 100]);
    assert_eq!(s.frame_at(0.0), 0);
    assert_eq!(s.frame_at(0.099), 0);
    assert_eq!(s.frame_at(0.100), 1);
    assert_eq!(s.frame_at(0.149), 1);
    assert_eq!(s.frame_at(0.150), 2);
    // Loops at total (250 ms) and clamps negatives to the first frame.
    assert_eq!(s.frame_at(0.250), 0);
    assert_eq!(s.frame_at(0.601), 1);
    assert_eq!(s.frame_at(-1.0), 0);
}

#[test]
fn static_stickers_always_show_frame_zero() {
    let s = seq(&[100]);
    assert_eq!(s.frame_at(12.34), 0);
}
