use cutlass_analysis::{
    MAX_SHOT_HISTOGRAM_BINS, MAX_SHOT_SAMPLE_HEIGHT, MAX_SHOT_SAMPLE_WIDTH, Rgba8Frame,
    Rgba8FrameError, ShotBoundaryKind, ShotDetectionConfig, ShotDetectionError, ShotDetector,
    ShotScoreWeights, detect_shots,
};

fn solid_rgba(width: usize, height: usize, rgba: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(width * height * 4);
    for _ in 0..width * height {
        bytes.extend_from_slice(&rgba);
    }
    bytes
}

fn checkerboard_rgba(width: usize, height: usize, inverted: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let bright = ((x + y) % 2 == 0) ^ inverted;
            let value = if bright { 255 } else { 0 };
            bytes.extend_from_slice(&[value, value, value, 255]);
        }
    }
    bytes
}

fn padded_checkerboard(
    width: usize,
    height: usize,
    row_stride: usize,
    inverted: bool,
    padding: u8,
) -> Vec<u8> {
    let mut bytes = vec![padding; row_stride * height];
    for y in 0..height {
        for x in 0..width {
            let bright = ((x + y) % 2 == 0) ^ inverted;
            let value = if bright { 255 } else { 0 };
            let pixel = y * row_stride + x * 4;
            bytes[pixel..pixel + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
    bytes
}

fn frame<'a>(
    bytes: &'a [u8],
    width: usize,
    height: usize,
    row_stride: usize,
    timestamp_seconds: f64,
) -> Rgba8Frame<'a> {
    Rgba8Frame::new(bytes, width, height, row_stride, timestamp_seconds)
        .expect("synthetic frame is valid")
}

fn test_config(threshold: f32, minimum_duration_seconds: f64) -> ShotDetectionConfig {
    ShotDetectionConfig::new(
        8,
        8,
        16,
        ShotScoreWeights::default(),
        threshold,
        minimum_duration_seconds,
    )
    .expect("test shot configuration is valid")
}

#[test]
fn frame_view_validates_metadata_without_copying() {
    let bytes = solid_rgba(2, 2, [10, 20, 30, 255]);
    let view = frame(&bytes, 2, 2, 8, 1.25);

    assert_eq!(view.bytes().as_ptr(), bytes.as_ptr());
    assert_eq!(view.bytes().len(), bytes.len());
    assert_eq!(view.width(), 2);
    assert_eq!(view.height(), 2);
    assert_eq!(view.row_stride(), 8);
    assert_eq!(view.visible_row_bytes(), 8);
    assert_eq!(view.timestamp_seconds(), 1.25);
}

#[test]
fn malformed_frame_buffers_and_arithmetic_overflow_are_rejected() {
    assert_eq!(
        Rgba8Frame::new(&[], 0, 1, 0, 0.0).err(),
        Some(Rgba8FrameError::ZeroWidth)
    );
    assert_eq!(
        Rgba8Frame::new(&[], 1, 0, 4, 0.0).err(),
        Some(Rgba8FrameError::ZeroHeight)
    );
    assert_eq!(
        Rgba8Frame::new(&[], usize::MAX, 1, usize::MAX, 0.0).err(),
        Some(Rgba8FrameError::RowBytesOverflow { width: usize::MAX })
    );
    assert_eq!(
        Rgba8Frame::new(&[0; 4], 2, 1, 4, 0.0).err(),
        Some(Rgba8FrameError::StrideTooSmall {
            row_stride: 4,
            minimum_row_stride: 8,
        })
    );
    assert_eq!(
        Rgba8Frame::new(&[], 1, usize::MAX, 4, 0.0).err(),
        Some(Rgba8FrameError::RequiredLengthOverflow {
            height: usize::MAX,
            row_stride: 4,
            visible_row_bytes: 4,
        })
    );
    assert_eq!(
        Rgba8Frame::new(&[0; 15], 2, 2, 8, 0.0).err(),
        Some(Rgba8FrameError::BufferTooShort {
            actual_length: 15,
            required_length: 16,
        })
    );
}

#[test]
fn nonfinite_and_negative_frame_timestamps_are_rejected() {
    let bytes = [0_u8; 4];
    assert_eq!(
        Rgba8Frame::new(&bytes, 1, 1, 4, f64::NAN).err(),
        Some(Rgba8FrameError::NonFiniteTimestamp)
    );
    assert_eq!(
        Rgba8Frame::new(&bytes, 1, 1, 4, f64::INFINITY).err(),
        Some(Rgba8FrameError::NonFiniteTimestamp)
    );
    assert_eq!(
        Rgba8Frame::new(&bytes, 1, 1, 4, -0.001).err(),
        Some(Rgba8FrameError::NegativeTimestamp)
    );
}

#[test]
fn shot_configuration_reports_precise_invalid_fields() {
    let error = ShotScoreWeights::new(f32::NAN, 1.0).expect_err("NaN weight must fail");
    assert_eq!(error.field(), "histogram_weight");
    assert_eq!(error.reason(), "must be finite and non-negative");

    let error = ShotScoreWeights::new(0.0, 0.0).expect_err("zero weights must fail");
    assert_eq!(error.field(), "score_weights");

    let weights = ShotScoreWeights::default();
    let invalid_cases = [
        ShotDetectionConfig::new(0, 1, 2, weights, 0.5, 0.0),
        ShotDetectionConfig::new(MAX_SHOT_SAMPLE_WIDTH + 1, 1, 2, weights, 0.5, 0.0),
        ShotDetectionConfig::new(1, 0, 2, weights, 0.5, 0.0),
        ShotDetectionConfig::new(1, MAX_SHOT_SAMPLE_HEIGHT + 1, 2, weights, 0.5, 0.0),
        ShotDetectionConfig::new(1, 1, 1, weights, 0.5, 0.0),
        ShotDetectionConfig::new(1, 1, MAX_SHOT_HISTOGRAM_BINS + 1, weights, 0.5, 0.0),
        ShotDetectionConfig::new(1, 1, 2, weights, 0.0, 0.0),
        ShotDetectionConfig::new(1, 1, 2, weights, f32::NAN, 0.0),
        ShotDetectionConfig::new(1, 1, 2, weights, 0.5, -0.1),
        ShotDetectionConfig::new(1, 1, 2, weights, 0.5, f64::INFINITY),
    ];
    assert!(invalid_cases.into_iter().all(|result| result.is_err()));

    let defaults = ShotDetectionConfig::default();
    assert_eq!(defaults.sample_width(), 64);
    assert_eq!(defaults.sample_height(), 36);
    assert_eq!(defaults.luma_histogram_bins(), 32);
    assert_eq!(defaults.score_weights().histogram_weight(), 0.4);
    assert_eq!(defaults.score_weights().pixel_difference_weight(), 0.6);
    assert_eq!(defaults.hard_cut_threshold(), 0.38);
    assert_eq!(defaults.minimum_shot_duration_seconds(), 0.5);
}

#[test]
fn identical_frames_and_sequence_end_do_not_create_boundaries() {
    let bytes = checkerboard_rgba(8, 8, false);
    let frames = [
        frame(&bytes, 8, 8, 32, 0.0),
        frame(&bytes, 8, 8, 32, 1.0),
        frame(&bytes, 8, 8, 32, 2.0),
    ];

    assert!(
        detect_shots(&frames[..1], ShotDetectionConfig::default())
            .expect("ordered frames")
            .is_empty()
    );
    assert!(
        detect_shots(&frames, ShotDetectionConfig::default())
            .expect("ordered frames")
            .is_empty()
    );
}

#[test]
fn stark_content_change_emits_boundary_at_current_frame() {
    let first = checkerboard_rgba(8, 8, false);
    let second = checkerboard_rgba(8, 8, true);
    let frames = [frame(&first, 8, 8, 32, 0.0), frame(&second, 8, 8, 32, 1.0)];

    let boundaries = detect_shots(&frames, ShotDetectionConfig::default()).expect("ordered frames");
    assert_eq!(boundaries.len(), 1);
    let boundary = boundaries[0];
    assert_eq!(boundary.kind(), ShotBoundaryKind::VisualChange);
    assert_eq!(boundary.timestamp_seconds(), 1.0);
    assert_eq!(boundary.previous_frame_timestamp_seconds(), 0.0);
    assert_eq!(boundary.current_frame_timestamp_seconds(), 1.0);
    assert!(boundary.score().is_finite());
    assert!((0.0..=1.0).contains(&boundary.score()));
    assert!(boundary.score() >= ShotDetectionConfig::default().hard_cut_threshold());
}

#[test]
fn gradual_global_brightness_fade_stays_below_hard_cut_threshold() {
    let images: Vec<Vec<u8>> = (0..=8)
        .map(|step| {
            let value = (step * 31) as u8;
            solid_rgba(8, 8, [value, value, value, 255])
        })
        .collect();
    let frames: Vec<_> = images
        .iter()
        .enumerate()
        .map(|(index, bytes)| frame(bytes, 8, 8, 32, index as f64 * 0.25))
        .collect();

    assert!(
        detect_shots(&frames, test_config(0.38, 0.0))
            .expect("ordered frames")
            .is_empty()
    );
}

#[test]
fn minimum_duration_suppresses_early_candidate_without_delaying_it() {
    let first = checkerboard_rgba(8, 8, false);
    let second = checkerboard_rgba(8, 8, true);
    let frames = [
        frame(&first, 8, 8, 32, 0.0),
        frame(&second, 8, 8, 32, 0.1),
        frame(&first, 8, 8, 32, 0.6),
    ];

    let boundaries = detect_shots(&frames, test_config(0.30, 0.5)).expect("ordered frames");
    assert_eq!(boundaries.len(), 1);
    assert_eq!(boundaries[0].timestamp_seconds(), 0.6);
    assert_eq!(boundaries[0].previous_frame_timestamp_seconds(), 0.1);
}

#[test]
fn padded_row_stride_is_honored_and_padding_is_ignored() {
    let first = padded_checkerboard(2, 2, 12, false, 0);
    let same_visible_pixels = padded_checkerboard(2, 2, 12, false, 255);
    let changed = padded_checkerboard(2, 2, 12, true, 17);
    let frames = [
        frame(&first, 2, 2, 12, 0.0),
        frame(&same_visible_pixels, 2, 2, 12, 1.0),
        frame(&changed, 2, 2, 12, 2.0),
    ];

    let boundaries = detect_shots(&frames, test_config(0.30, 0.0)).expect("ordered frames");
    assert_eq!(boundaries.len(), 1);
    assert_eq!(boundaries[0].timestamp_seconds(), 2.0);
}

#[test]
fn dimension_change_is_a_high_confidence_boundary_candidate() {
    let small = solid_rgba(2, 2, [80, 80, 80, 255]);
    let wide = solid_rgba(3, 2, [80, 80, 80, 255]);
    let frames = [
        frame(&small, 2, 2, 8, 0.0),
        frame(&wide, 3, 2, 12, 1.0),
        frame(&wide, 3, 2, 12, 2.0),
    ];

    let boundaries = detect_shots(&frames, ShotDetectionConfig::default()).expect("ordered frames");
    assert_eq!(boundaries.len(), 1);
    assert_eq!(boundaries[0].kind(), ShotBoundaryKind::DimensionChange);
    assert_eq!(boundaries[0].score(), 1.0);
    assert_eq!(boundaries[0].timestamp_seconds(), 1.0);
}

#[test]
fn duplicate_timestamps_are_accepted_and_remain_sorted() {
    let first = checkerboard_rgba(8, 8, false);
    let second = checkerboard_rgba(8, 8, true);
    let frames = [frame(&first, 8, 8, 32, 0.0), frame(&second, 8, 8, 32, 0.0)];

    let boundaries =
        detect_shots(&frames, test_config(0.30, 0.0)).expect("duplicate timestamps are valid");
    assert_eq!(boundaries.len(), 1);
    assert_eq!(boundaries[0].previous_frame_timestamp_seconds(), 0.0);
    assert_eq!(boundaries[0].current_frame_timestamp_seconds(), 0.0);
}

#[test]
fn out_of_order_timestamp_fails_closed_without_mutating_streaming_state() {
    let bytes = checkerboard_rgba(4, 4, false);
    let first = frame(&bytes, 4, 4, 16, 1.0);
    let out_of_order = frame(&bytes, 4, 4, 16, 0.5);
    let next = frame(&bytes, 4, 4, 16, 2.0);
    let mut detector = ShotDetector::new(test_config(0.30, 0.0));

    assert_eq!(detector.push_frame(first), Ok(None));
    assert_eq!(
        detector.push_frame(out_of_order),
        Err(ShotDetectionError::OutOfOrderTimestamp {
            previous_timestamp_seconds: 1.0,
            current_timestamp_seconds: 0.5,
        })
    );
    assert_eq!(detector.push_frame(next), Ok(None));

    assert_eq!(
        detect_shots([first, out_of_order], test_config(0.30, 0.0)),
        Err(ShotDetectionError::OutOfOrderTimestamp {
            previous_timestamp_seconds: 1.0,
            current_timestamp_seconds: 0.5,
        })
    );
}

#[test]
fn one_shot_and_streaming_detection_are_identical() {
    let first = checkerboard_rgba(8, 8, false);
    let second = checkerboard_rgba(8, 8, true);
    let third = solid_rgba(8, 8, [255, 0, 0, 255]);
    let frames = [
        frame(&first, 8, 8, 32, 0.0),
        frame(&second, 8, 8, 32, 1.0),
        frame(&third, 8, 8, 32, 2.0),
        frame(&third, 8, 8, 32, 3.0),
    ];
    let config = test_config(0.25, 0.0);
    let one_shot = detect_shots(&frames, config).expect("ordered frames");

    let mut detector = ShotDetector::new(config);
    let streaming: Vec<_> = frames
        .iter()
        .filter_map(|&frame| detector.push_frame(frame).expect("ordered frames"))
        .collect();

    assert_eq!(one_shot, streaming);
}

#[test]
fn repeated_detection_is_bit_for_bit_deterministic_and_sorted() {
    let first = checkerboard_rgba(8, 8, false);
    let second = checkerboard_rgba(8, 8, true);
    let frames = [
        frame(&first, 8, 8, 32, 0.0),
        frame(&second, 8, 8, 32, 1.0),
        frame(&first, 8, 8, 32, 2.0),
        frame(&second, 8, 8, 32, 3.0),
    ];
    let config = test_config(0.30, 0.0);
    let first_run = detect_shots(&frames, config).expect("ordered frames");
    let second_run = detect_shots(&frames, config).expect("ordered frames");

    assert_eq!(first_run, second_run);
    assert!(
        first_run
            .windows(2)
            .all(|pair| pair[0].timestamp_seconds() <= pair[1].timestamp_seconds())
    );
    assert!(
        first_run.iter().all(
            |boundary| boundary.timestamp_seconds().is_finite() && boundary.score().is_finite()
        )
    );
}

#[test]
fn feature_storage_remains_bounded_by_configuration_not_source_size() {
    let config = ShotDetectionConfig::new(3, 2, 8, ShotScoreWeights::default(), 0.30, 0.0)
        .expect("valid bounded config");
    let bytes = checkerboard_rgba(40, 30, false);
    let first = frame(&bytes, 40, 30, 160, 0.0);
    let second = frame(&bytes, 40, 30, 160, 1.0);
    let mut detector = ShotDetector::new(config);

    assert_eq!(config.maximum_sampled_pixels(), 6);
    assert_eq!(detector.sampled_pixel_storage_limit(), 12);
    assert_eq!(detector.histogram_bin_storage_limit(), 16);
    detector.push_frame(first).expect("ordered first frame");
    assert_eq!(detector.retained_sampled_pixels(), 6);
    detector.push_frame(second).expect("ordered second frame");
    assert_eq!(detector.retained_sampled_pixels(), 12);
    assert!(detector.retained_sampled_pixels() <= detector.sampled_pixel_storage_limit());

    detector.reset();
    assert_eq!(detector.retained_sampled_pixels(), 0);
}
