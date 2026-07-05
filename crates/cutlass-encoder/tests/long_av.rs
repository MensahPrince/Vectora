//! Long-form A/V regression: with both a video and an audio input,
//! `AVAssetWriter` gates each input's readiness on its interleaving pattern,
//! which used to wedge the single-threaded export loop after ~1 s of media
//! ("encoder input never became ready"). The queue-and-pump encoder must
//! sustain arbitrarily long interleaved pushes.

#![cfg(target_vendor = "apple")]

use std::time::{SystemTime, UNIX_EPOCH};

use cutlass_core::{
    AudioEncoderConfig, ColorSpace, CpuImage, EncoderConfig, FrameData, PixelFormat, Plane,
    Rational, RationalTime, Rect, Rotation, VideoEncoder, VideoFrame,
};
use cutlass_encoder::AvfEncoder;

fn solid_rgba_frame(width: u32, height: u32, index: i64, rate: Rational) -> VideoFrame {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..(width * height) {
        pixels.extend_from_slice(&[20, 120, 220, 255]);
    }
    let plane = Plane::new(pixels, width as usize * 4, height as usize);
    VideoFrame::new(
        RationalTime::new(index, rate),
        PixelFormat::Rgba8,
        ColorSpace::BT709,
        (width, height),
        Rect::from_size(width, height),
        Rotation::None,
        FrameData::Cpu(CpuImage::new(vec![plane])),
    )
}

fn temp_mp4(tag: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("cutlass_enc_{tag}_{nanos}.mp4"))
}

/// 10 s of interleaved video + audio (the export loop's push pattern).
#[test]
fn interleaved_av_export_sustains_ten_seconds() {
    let (width, height) = (320u32, 240u32);
    let rate = Rational::FPS_30;
    let audio_rate = 48_000u32;
    let path = temp_mp4("av300");

    let config = EncoderConfig::new((width, height), rate, ColorSpace::BT709)
        .with_audio(AudioEncoderConfig::new(audio_rate, 2));
    let mut encoder = AvfEncoder::open(&path, config).expect("open encoder");

    let per_frame = (audio_rate / 30) as usize;
    let mut cursor = 0i64;
    for i in 0..300i64 {
        encoder
            .push(&solid_rgba_frame(width, height, i, rate))
            .unwrap_or_else(|e| panic!("push frame {i}: {e}"));
        let block: Vec<f32> = vec![0.1; per_frame * 2];
        let pts = RationalTime::new(cursor, Rational::new(audio_rate as i32, 1));
        encoder
            .push_audio(&block, pts)
            .unwrap_or_else(|e| panic!("push audio {i}: {e}"));
        cursor += per_frame as i64;
    }
    encoder.finish().expect("finish");

    let meta = std::fs::metadata(&path).expect("output exists");
    assert!(meta.len() > 0, "encoded file is empty");
    let probed = cutlass_decoder::probe(&path).expect("probe output");
    assert!(probed.has_audio, "output should carry an audio track");
    assert_eq!(probed.frame_count, 300, "all video frames should land");

    let _ = std::fs::remove_file(&path);
}
