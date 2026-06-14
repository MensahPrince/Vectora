//! Integration tests: probe agrees with the decoder on real assets.

mod common;

use common::{any_mp4_in_assets, small_video_asset};
use cutlass_decoder::{DecodeOptions, Decoder, HwAccel};
use cutlass_probe::probe;

#[test]
fn init_smoke() {
    cutlass_probe::init();
}

#[test]
fn probe_matches_decoder_metadata() {
    let Some(path) = small_video_asset().or_else(any_mp4_in_assets) else {
        return;
    };

    let probed = probe(&path).expect("probe");
    let dec = Decoder::open_with(&path, DecodeOptions::default().hw_accel(HwAccel::None))
        .expect("open decoder");

    let info = dec.info();
    assert_eq!(probed.width, info.width);
    assert_eq!(probed.height, info.height);

    let (num, den) = info.frame_rate_parts();
    assert_eq!(probed.frame_rate, cutlass_models::Rational::new(num, den));

    if let Some(duration) = dec.duration() {
        let micros = duration.as_micros() as u64;
        let expected = cutlass_probe::duration_ticks_from_micros(probed.frame_rate, micros);
        assert_eq!(probed.duration_ticks, expected);
    }
}

#[test]
fn probe_reports_positive_duration_for_asset() {
    let Some(path) = small_video_asset().or_else(any_mp4_in_assets) else {
        return;
    };
    let probed = probe(&path).expect("probe");
    assert!(probed.duration_ticks > 0);
    assert!(probed.width > 0);
    assert!(probed.height > 0);
    assert!(!probed.video_codec.is_empty());
}

#[test]
fn missing_file_errors() {
    let err = probe(std::path::Path::new("/no/such/file.mp4")).unwrap_err();
    assert!(matches!(err, cutlass_probe::ProbeError::Open(_)));
}
