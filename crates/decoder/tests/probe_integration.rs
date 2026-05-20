//! Integration tests for `decoder::probe`: real FFmpeg I/O against checked-in
//! fixtures under `tests/assets/`. Regenerate with `tests/assets/regenerate.sh`.
//!
//! Probe is the metadata-only path used by import (no codec open), so these
//! tests deliberately exercise files the *decoder* would reject (`ffv1` pixel
//! format) — probe must still surface stream geometry so the import UI can
//! render an informative tile rather than a silent failure.

#![cfg(unix)]

mod common;

use std::path::PathBuf;

use common::asset;

use decoder::{DecoderError, ProbedKind, Rational, probe};

// --- Duration tolerance helpers --------------------------------------------
//
// FFmpeg's container-reported `duration` field is built from the demuxer's
// estimate of the longest stream and is often a frame or two off the nominal
// content length: a 5 s clip can report 4.967 s or 5.033 s depending on the
// trailing PTS. Bracketing with `Rational::ge` avoids float drift while still
// being precise enough to catch a genuinely wrong duration (e.g. 0 or 50 s).
//
// Per repo convention we never compare media times via `f64`.

fn assert_duration_between(d: Rational, low_secs: u32, high_secs: u32) {
    assert!(
        d.ge(Rational::new_raw(i64::from(low_secs), 1)),
        "expected duration ≥ {low_secs}s, got {d}"
    );
    assert!(
        !d.ge(Rational::new_raw(i64::from(high_secs), 1)),
        "expected duration < {high_secs}s, got {d}"
    );
}

// --- Single-stream files ---------------------------------------------------

#[test]
fn probe_video_only_h264() {
    let p = probe(&asset("testsrc_h264.mp4")).expect("probe");

    assert_eq!(p.kind, ProbedKind::Video);
    assert!(p.audio.is_none(), "fixture has no audio");

    let v = p.video.expect("h264 video stream");
    assert_eq!(v.width, 320);
    assert_eq!(v.height, 240);
    assert_eq!(v.codec, "h264", "expected ffmpeg short name 'h264'");

    // testsrc → libx264 emits avg_frame_rate=30/1 exactly; bracket on
    // [29, 31) absorbs any container-side rounding without admitting nonsense.
    let fps = v.fps;
    assert!(
        fps.ge(Rational::new_raw(29, 1)) && !fps.ge(Rational::new_raw(31, 1)),
        "expected fps ≈ 30/1, got {fps}"
    );

    // 5 s fixture; tolerate ±1 frame at 30 fps (~33 ms).
    let d = p.duration.expect("mp4 container reports duration");
    assert_duration_between(d, 4, 6);
}

#[test]
fn probe_audio_only_aac() {
    let p = probe(&asset("audio_only.m4a")).expect("probe");

    assert_eq!(p.kind, ProbedKind::Audio);
    assert!(p.video.is_none(), "fixture has no video");

    let a = p.audio.expect("aac audio stream");
    assert_eq!(a.sample_rate, 48_000);
    assert_eq!(a.codec, "aac");

    // 2 s fixture; AAC pads to the next packet, so we expect 2.x s — allow up
    // to 3 s, but require strictly > 1 s.
    let d = p.duration.expect("m4a container reports duration");
    assert_duration_between(d, 1, 3);
}

// --- Combined A/V ----------------------------------------------------------

#[test]
fn probe_combined_av() {
    let p = probe(&asset("test_av.mp4")).expect("probe");

    assert_eq!(p.kind, ProbedKind::Video);

    let v = p.video.expect("video stream");
    assert_eq!(v.width, 128);
    assert_eq!(v.height, 96);
    assert_eq!(v.codec, "h264");

    let fps = v.fps;
    assert!(
        fps.ge(Rational::new_raw(23, 1)) && !fps.ge(Rational::new_raw(25, 1)),
        "expected fps ≈ 24/1, got {fps}"
    );

    let a = p.audio.expect("audio stream");
    assert_eq!(a.sample_rate, 48_000);
    assert_eq!(a.codec, "aac");

    let d = p.duration.expect("mp4 reports duration");
    assert_duration_between(d, 1, 3);
}

// --- Probe must be permissive: it's a metadata read, not a "can we decode" check.

#[test]
fn probe_unsupported_codec_still_succeeds() {
    // `Decoder::open` rejects this file (BGR0 pixel format outside the v1
    // allowlist) — see `decode_integration::open_unsupported_pixel_format`.
    // The import path should still get full geometry so the user sees the
    // file with an "unsupported codec" badge instead of nothing at all.
    let p = probe(&asset("test_unsupported_codec.mkv")).expect("probe succeeds for ffv1");

    assert_eq!(p.kind, ProbedKind::Video);
    let v = p.video.expect("video stream");
    assert_eq!(v.codec, "ffv1");
    assert_eq!(v.width, 64);
    assert_eq!(v.height, 48);

    let fps = v.fps;
    assert!(
        fps.ge(Rational::new_raw(11, 1)) && !fps.ge(Rational::new_raw(13, 1)),
        "expected fps ≈ 12/1, got {fps}"
    );
    assert!(p.audio.is_none());
}

// --- Image kind: 2×2 PNG demuxed as a 1-frame video stream -----------------

#[test]
fn probe_image_png_classifies_as_image() {
    let p = probe(&asset("testsrc_image.png")).expect("probe");

    assert_eq!(
        p.kind,
        ProbedKind::Image,
        "png_pipe demuxer must surface as Image, not Video"
    );

    let v = p.video.expect("png exposed as 1-frame video stream");
    assert_eq!(v.width, 2);
    assert_eq!(v.height, 2);
    assert_eq!(v.codec, "png");
    assert!(p.audio.is_none());

    // `png_pipe` doesn't carry container duration — probe must surface that
    // as `None` rather than fabricating a value.
    assert!(
        p.duration.is_none(),
        "png_pipe should report no container duration, got {:?}",
        p.duration
    );
}

// --- Negative paths --------------------------------------------------------

#[test]
fn probe_corrupt_file_errors() {
    let res = probe(&asset("corrupt_truncated.mp4"));
    match res {
        Err(DecoderError::Open(_)) => {}
        Err(e) => panic!("expected DecoderError::Open, got {e:?}"),
        Ok(p) => panic!("expected error, got {p:?}"),
    }
}

#[test]
fn probe_nonexistent_file_errors() {
    // Path with a stream-bin-style component that no test process should ever
    // race against; we don't need uniqueness — just a path the OS refuses.
    let nowhere = PathBuf::from("/no/such/dir/cutlass_probe_missing_xyz.mp4");
    let res = probe(&nowhere);
    match res {
        Err(DecoderError::Open(_)) => {}
        Err(e) => panic!("expected DecoderError::Open, got {e:?}"),
        Ok(p) => panic!("expected error, got {p:?}"),
    }
}

// --- FFI safety regression: `avcodec_get_name` returns a static C string;
// we copy into an owned `String`. Calling probe twice must not return a
// different (or moved) buffer, and must not corrupt earlier results.

#[test]
fn probe_codec_name_is_stable_across_calls() {
    let p1 = probe(&asset("test_av.mp4")).expect("probe 1");
    let p2 = probe(&asset("test_av.mp4")).expect("probe 2");

    let v1 = p1.video.as_ref().expect("v1");
    let v2 = p2.video.as_ref().expect("v2");
    assert_eq!(v1.codec, v2.codec, "video codec name must be stable");
    assert_eq!(v1.codec, "h264");
    assert_eq!(v1.width, v2.width);
    assert_eq!(v1.height, v2.height);

    let a1 = p1.audio.as_ref().expect("a1");
    let a2 = p2.audio.as_ref().expect("a2");
    assert_eq!(a1.codec, a2.codec, "audio codec name must be stable");
    assert_eq!(a1.codec, "aac");
    assert_eq!(a1.sample_rate, a2.sample_rate);
}

// --- Cross-fixture sanity: mixing different codecs in one process tests that
// the static C-string returned by `avcodec_get_name` for one call isn't
// later overwritten when called for a different codec ID (it shouldn't be —
// the strings are program-lifetime — but the assertion is cheap insurance).

#[test]
fn probe_codec_names_distinct_for_different_files() {
    let h264 = probe(&asset("testsrc_h264.mp4")).expect("probe h264");
    let ffv1 = probe(&asset("test_unsupported_codec.mkv")).expect("probe ffv1");
    let png = probe(&asset("testsrc_image.png")).expect("probe png");

    assert_eq!(h264.video.as_ref().unwrap().codec, "h264");
    assert_eq!(ffv1.video.as_ref().unwrap().codec, "ffv1");
    assert_eq!(png.video.as_ref().unwrap().codec, "png");
}
