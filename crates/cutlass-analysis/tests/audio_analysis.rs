use std::f32::consts::TAU;

use cutlass_analysis::{
    AudioInputError, BeatConfig, InterleavedPcm, LoudnessConfig, SilenceConfig, TimeSpan,
    detect_beats, detect_silence, loudness_contour,
};

fn loudness_config(window: f64, hop: f64) -> LoudnessConfig {
    LoudnessConfig::new(window, hop, -100.0).expect("test loudness config is valid")
}

fn silence_config(
    loudness: LoudnessConfig,
    threshold_dbfs: f32,
    minimum_duration_seconds: f64,
    merge_gap_seconds: f64,
    hysteresis_db: f32,
) -> SilenceConfig {
    SilenceConfig::new(
        loudness,
        threshold_dbfs,
        minimum_duration_seconds,
        merge_gap_seconds,
        hysteresis_db,
    )
    .expect("test silence config is valid")
}

fn beat_config(loudness: LoudnessConfig, refractory_seconds: f64) -> BeatConfig {
    BeatConfig::new(loudness, 0.5, 1.5, 3.0, refractory_seconds).expect("test beat config is valid")
}

fn append_constant(samples: &mut Vec<f32>, frames: usize, amplitude: f32) {
    samples.extend(std::iter::repeat_n(amplitude, frames));
}

fn pulse_train(
    sample_rate: u32,
    frame_count: usize,
    pulse_starts_seconds: &[f64],
    pulse_duration_seconds: f64,
) -> Vec<f32> {
    let mut samples = vec![0.0; frame_count];
    let pulse_frames = (pulse_duration_seconds * f64::from(sample_rate)).round() as usize;
    for &start_seconds in pulse_starts_seconds {
        let start_frame = (start_seconds * f64::from(sample_rate)).round() as usize;
        let end_frame = start_frame.saturating_add(pulse_frames).min(frame_count);
        samples[start_frame..end_frame].fill(1.0);
    }
    samples
}

fn assert_near(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {actual} to be within {tolerance} of {expected}"
    );
}

#[test]
fn invalid_pcm_metadata_alignment_and_values_are_rejected() {
    assert_eq!(
        InterleavedPcm::new(&[], 0, 1).err(),
        Some(AudioInputError::ZeroSampleRate)
    );
    assert_eq!(
        InterleavedPcm::new(&[], 48_000, 0).err(),
        Some(AudioInputError::ZeroChannels)
    );
    assert_eq!(
        InterleavedPcm::new(&[0.0, 0.0, 0.0], 48_000, 2).err(),
        Some(AudioInputError::NonFrameAligned {
            sample_count: 3,
            channels: 2,
        })
    );
    assert_eq!(
        InterleavedPcm::new(&[0.0, f32::NAN], 48_000, 1).err(),
        Some(AudioInputError::NonFiniteSample { index: 1 })
    );
    assert_eq!(
        InterleavedPcm::new(&[f32::INFINITY], 48_000, 1).err(),
        Some(AudioInputError::NonFiniteSample { index: 0 })
    );
    assert_eq!(
        loudness_contour(&[f32::NEG_INFINITY], 48_000, 1, LoudnessConfig::default()).err(),
        Some(AudioInputError::NonFiniteSample { index: 0 })
    );
}

#[test]
fn invalid_configs_are_rejected_at_construction() {
    assert!(LoudnessConfig::new(0.0, 0.01, -100.0).is_err());
    assert!(LoudnessConfig::new(0.02, f64::NAN, -100.0).is_err());
    assert!(LoudnessConfig::new(0.01, 0.02, -100.0).is_err());
    assert!(LoudnessConfig::new(0.02, 0.01, 0.0).is_err());
    assert!(LoudnessConfig::new(0.02, 0.01, f32::NEG_INFINITY).is_err());

    let loudness = loudness_config(0.02, 0.01);
    assert!(SilenceConfig::new(loudness, -101.0, 0.1, 0.0, 0.0).is_err());
    assert!(SilenceConfig::new(loudness, 0.0, 0.1, 0.0, 0.0).is_err());
    assert!(SilenceConfig::new(loudness, -50.0, -0.1, 0.0, 0.0).is_err());
    assert!(SilenceConfig::new(loudness, -50.0, 0.1, f64::INFINITY, 0.0).is_err());
    assert!(SilenceConfig::new(loudness, -2.0, 0.1, 0.0, 3.0).is_err());

    assert!(BeatConfig::new(loudness, 0.0, 1.0, 3.0, 0.1).is_err());
    assert!(BeatConfig::new(loudness, 0.5, -1.0, 3.0, 0.1).is_err());
    assert!(BeatConfig::new(loudness, 0.5, 1.0, 0.0, 0.1).is_err());
    assert!(BeatConfig::new(loudness, 0.5, 1.0, 3.0, -0.1).is_err());
}

#[test]
fn empty_pcm_is_valid_and_all_analyses_are_empty() {
    let pcm = InterleavedPcm::new(&[], 48_000, 2).expect("empty PCM is valid");
    assert!(pcm.is_empty());
    assert_eq!(pcm.frame_count(), 0);
    assert_eq!(pcm.duration_seconds(), 0.0);
    assert!(pcm.loudness_contour(LoudnessConfig::default()).is_empty());
    assert!(pcm.detect_silence(SilenceConfig::default()).is_empty());
    assert!(pcm.detect_beats(BeatConfig::default()).is_empty());
}

#[test]
fn pure_silence_has_a_finite_floor_and_one_full_silence_span() {
    let samples = vec![0.0; 1_000];
    let loudness = loudness_config(0.1, 0.05);
    let contour = loudness_contour(&samples, 1_000, 1, loudness).expect("valid PCM");

    assert!(!contour.is_empty());
    assert!(contour.iter().all(|point| point.dbfs == -100.0));
    assert!(contour.iter().all(|point| point.dbfs.is_finite()));

    let spans = detect_silence(
        &samples,
        1_000,
        1,
        silence_config(loudness, -50.0, 0.2, 0.05, 3.0),
    )
    .expect("valid PCM");
    assert_eq!(spans.len(), 1);
    assert_eq!(
        spans[0].span,
        TimeSpan::new(0.0, 1.0).expect("valid expected span")
    );

    let floor_threshold_spans = detect_silence(
        &samples,
        1_000,
        1,
        silence_config(loudness, -100.0, 0.0, 0.0, 0.0),
    )
    .expect("valid PCM");
    assert_eq!(floor_threshold_spans, spans);

    let beats = detect_beats(&samples, 1_000, 1, beat_config(loudness, 0.1)).expect("valid PCM");
    assert!(beats.is_empty());
}

#[test]
fn loudness_is_finite_and_clamped_to_floor_and_zero_dbfs() {
    let loudness = loudness_config(0.01, 0.01);
    let hot = vec![f32::MAX; 20];
    let tiny = vec![f32::MIN_POSITIVE; 20];

    let hot_contour = loudness_contour(&hot, 1_000, 1, loudness).expect("valid hot PCM");
    let tiny_contour = loudness_contour(&tiny, 1_000, 1, loudness).expect("valid tiny PCM");

    assert!(
        hot_contour
            .iter()
            .all(|point| point.dbfs == 0.0 && point.dbfs.is_finite())
    );
    assert!(
        tiny_contour
            .iter()
            .all(|point| point.dbfs == -100.0 && point.dbfs.is_finite())
    );
}

#[test]
fn opposite_polarity_stereo_uses_per_channel_energy_without_cancellation() {
    let mut samples = Vec::with_capacity(2_000);
    for _ in 0..1_000 {
        samples.extend_from_slice(&[0.5, -0.5]);
    }
    let loudness = loudness_config(0.1, 0.05);
    let contour = loudness_contour(&samples, 1_000, 2, loudness).expect("valid stereo");

    assert!(
        contour
            .iter()
            .all(|point| (point.dbfs + 6.020_600_3).abs() < 0.000_1)
    );
    let silent = detect_silence(
        &samples,
        1_000,
        2,
        silence_config(loudness, -20.0, 0.1, 0.0, 2.0),
    )
    .expect("valid stereo");
    assert!(silent.is_empty());
}

#[test]
fn silence_detection_merges_short_loud_gap_before_minimum_duration_filter() {
    let mut samples = Vec::new();
    append_constant(&mut samples, 200, 0.5);
    append_constant(&mut samples, 200, 0.0);
    append_constant(&mut samples, 20, 0.5);
    append_constant(&mut samples, 200, 0.0);
    append_constant(&mut samples, 200, 0.5);

    let loudness = loudness_config(0.01, 0.01);
    let without_merge = detect_silence(
        &samples,
        1_000,
        1,
        silence_config(loudness, -40.0, 0.3, 0.0, 2.0),
    )
    .expect("valid PCM");
    assert!(without_merge.is_empty());

    let merged = detect_silence(
        &samples,
        1_000,
        1,
        silence_config(loudness, -40.0, 0.3, 0.05, 2.0),
    )
    .expect("valid PCM");
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged[0].span,
        TimeSpan::new(0.2, 0.62).expect("valid expected span")
    );
}

#[test]
fn silence_hysteresis_prevents_chatter_between_entry_and_exit_levels() {
    let amplitude_at_db = |dbfs: f32| 10.0_f32.powf(dbfs / 20.0);
    let mut samples = Vec::new();
    append_constant(&mut samples, 100, amplitude_at_db(-20.0));
    append_constant(&mut samples, 100, amplitude_at_db(-60.0));
    append_constant(&mut samples, 100, amplitude_at_db(-48.0));
    append_constant(&mut samples, 100, amplitude_at_db(-40.0));

    let spans = detect_silence(
        &samples,
        1_000,
        1,
        silence_config(loudness_config(0.1, 0.1), -50.0, 0.0, 0.0, 5.0),
    )
    .expect("valid PCM");

    assert_eq!(spans.len(), 1);
    assert_eq!(
        spans[0].span,
        TimeSpan::new(0.1, 0.3).expect("valid expected span")
    );
}

#[test]
fn regular_pulse_train_recovers_one_event_per_pulse_with_tolerance() {
    let expected = [0.25, 0.75, 1.25, 1.75, 2.25, 2.75];
    let samples = pulse_train(1_000, 3_000, &expected, 0.02);
    let config = beat_config(loudness_config(0.02, 0.01), 0.12);
    let events = detect_beats(&samples, 1_000, 1, config).expect("valid PCM");

    assert_eq!(events.len(), expected.len(), "{events:?}");
    for (event, expected_seconds) in events.iter().zip(expected) {
        assert_near(event.time_seconds, expected_seconds, 0.03);
        assert!(event.strength_db.is_finite());
        assert!(event.strength_db >= config.minimum_onset_db());
    }
}

#[test]
fn constant_energy_and_constant_tone_do_not_emit_false_beats() {
    let constant = vec![0.4; 4_800];
    let sine: Vec<f32> = (0..47_777)
        .map(|frame| {
            let phase = TAU * 440.0 * frame as f32 / 48_000.0;
            0.4 * phase.sin()
        })
        .collect();
    let config = BeatConfig::new(
        LoudnessConfig::new(0.02, 0.01, -120.0).expect("valid loudness"),
        0.5,
        1.5,
        3.0,
        0.1,
    )
    .expect("valid beat config");

    assert!(
        detect_beats(&constant, 4_800, 1, config)
            .expect("valid constant PCM")
            .is_empty()
    );
    assert!(
        detect_beats(&sine, 48_000, 1, config)
            .expect("valid sine PCM")
            .is_empty()
    );
}

#[test]
fn refractory_interval_suppresses_nearby_onsets() {
    let samples = pulse_train(1_000, 800, &[0.20, 0.25, 0.50], 0.01);
    let events = detect_beats(
        &samples,
        1_000,
        1,
        beat_config(loudness_config(0.01, 0.005), 0.10),
    )
    .expect("valid PCM");

    assert_eq!(events.len(), 2, "{events:?}");
    assert_near(events[0].time_seconds, 0.20, 0.02);
    assert_near(events[1].time_seconds, 0.50, 0.02);
}

#[test]
fn repeat_analysis_is_bit_for_bit_deterministic() {
    let samples = pulse_train(2_000, 4_000, &[0.2, 0.7, 1.2, 1.7], 0.015);
    let loudness = loudness_config(0.02, 0.01);
    let silence = silence_config(loudness, -45.0, 0.1, 0.03, 3.0);
    let beats = beat_config(loudness, 0.1);

    assert_eq!(
        loudness_contour(&samples, 2_000, 1, loudness).expect("valid PCM"),
        loudness_contour(&samples, 2_000, 1, loudness).expect("valid PCM")
    );
    assert_eq!(
        detect_silence(&samples, 2_000, 1, silence).expect("valid PCM"),
        detect_silence(&samples, 2_000, 1, silence).expect("valid PCM")
    );
    assert_eq!(
        detect_beats(&samples, 2_000, 1, beats).expect("valid PCM"),
        detect_beats(&samples, 2_000, 1, beats).expect("valid PCM")
    );
}

#[test]
fn long_input_has_window_proportional_output() {
    let samples = vec![0.25; 1_000_000];
    let loudness = loudness_config(0.5, 0.5);
    let contour = loudness_contour(&samples, 1_000, 1, loudness).expect("valid PCM");
    let beats = detect_beats(&samples, 1_000, 1, beat_config(loudness, 0.1)).expect("valid PCM");

    assert_eq!(contour.len(), 2_000);
    assert!(beats.is_empty());
    assert_eq!(
        contour.last().expect("non-empty").span.end_seconds(),
        1_000.0
    );
}
