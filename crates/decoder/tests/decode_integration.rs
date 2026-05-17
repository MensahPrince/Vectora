//! Integration tests: real FFmpeg I/O against checked-in fixtures under `tests/assets/`.
//! Regenerate with `tests/assets/regenerate.sh` (requires `ffmpeg` on PATH).

#![cfg(unix)]

mod common;

use std::path::PathBuf;

use common::{asset, assert_yuv420p_layout};

use decoder::{DecodeOutcome, Decoder, DecoderError, PixelFormat, Rational};

// --- Open / probe ---

#[test]
fn open_probe_h264_yuv420p() {
    let dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let info = dec.info();
    assert_eq!(info.width, 320);
    assert_eq!(info.height, 240);
    assert_eq!(info.pixel_format, PixelFormat::Yuv420p);
    assert!(info.timebase.den > 0);
    let d = info.duration.expect("fixture reports duration");
    assert!(
        d.ge(Rational::new_raw(4, 1)) && !d.ge(Rational::new_raw(6, 1)),
        "duration ~5s, got {d}"
    );
}

#[test]
fn open_missing_file_errors() {
    let res = Decoder::open(PathBuf::from("/no/such/cutlass_video_xyz.mp4").as_path());
    match res {
        Err(DecoderError::Open(_)) => {}
        Ok(_) => panic!("expected open error"),
        Err(e) => panic!("unexpected error: {e}"),
    }
}

#[test]
fn open_audio_only_no_video_stream() {
    let res = Decoder::open(&asset("audio_only.m4a"));
    match res {
        Err(DecoderError::Unsupported { what }) => {
            assert!(
                what.contains("no video stream"),
                "unexpected message: {what}"
            );
        }
        Ok(_) => panic!("expected no video stream"),
        Err(e) => panic!("unexpected: {e}"),
    }
}

#[test]
fn open_unsupported_pixel_format() {
    let res = Decoder::open(&asset("test_unsupported_codec.mkv"));
    match res {
        Err(DecoderError::Unsupported { what }) => {
            assert!(
                what.contains("pixel format") || what.contains("YUV420P"),
                "unexpected: {what}"
            );
        }
        Ok(_) => panic!("expected unsupported pixel format"),
        Err(e) => panic!("unexpected: {e}"),
    }
}

#[test]
fn open_corrupt_truncated_demuxer_fails() {
    let res = Decoder::open(&asset("corrupt_truncated.mp4"));
    assert!(
        matches!(&res, Err(DecoderError::Open(_))),
        "expected Open error, got {:?}",
        res.as_ref().err()
    );
}

#[cfg(unix)]
#[test]
fn open_rejects_non_utf8_path() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let p = PathBuf::from(OsString::from_vec(vec![b't', b'm', b'p', 0xFF, b'.', b'm', b'p', b'4']));
    let res = Decoder::open(&p);
    match res {
        Err(DecoderError::Unsupported { what }) if what.contains("UTF-8") => {}
        Err(e) => panic!("expected UTF-8 unsupported path error, got {e}"),
        Ok(_) => panic!("expected error"),
    }
}

// --- Sequential decode ---

#[test]
fn decode_all_frames_count_and_stride() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let mut n = 0u32;
    let mut last_pts = Rational::new_raw(i64::MIN, 1);
    loop {
        match dec.next_frame().expect("next") {
            DecodeOutcome::Eof => break,
            DecodeOutcome::Frame(f) => {
                if n > 0 {
                    assert!(f.pts.ge(last_pts), "PTS not monotonic");
                }
                last_pts = f.pts;
                assert_yuv420p_layout(&f, 320, 240);
                assert_eq!(f.timebase, dec.info().timebase);
                n += 1;
            }
        }
    }
    assert_eq!(n, 150, "5s * 30fps = 150 frames");
}

#[test]
fn decode_mixed_av_file_skips_audio_packets() {
    let mut dec = Decoder::open(&asset("test_av.mp4")).expect("open");
    assert_eq!(dec.info().width, 128);
    assert_eq!(dec.info().height, 96);
    let mut n = 0u32;
    loop {
        match dec.next_frame().expect("next") {
            DecodeOutcome::Eof => break,
            DecodeOutcome::Frame(f) => {
                assert_yuv420p_layout(&f, 128, 96);
                n += 1;
            }
        }
    }
    assert_eq!(n, 48, "2s * 24fps");
}

#[test]
fn first_frame_starts_near_zero() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(f) = dec.next_frame().expect("next") else {
        panic!("expected frame");
    };
    let z = Rational::new_raw(0, 1);
    assert!(
        f.pts.ge(z),
        "first pts should be >= 0, got {:?}",
        f.pts
    );
    assert!(
        !f.pts.ge(Rational::new_raw(1, 10)),
        "first frame should be near start"
    );
}

#[test]
fn eof_then_next_stays_eof_until_seek() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    loop {
        match dec.next_frame().expect("next") {
            DecodeOutcome::Eof => break,
            DecodeOutcome::Frame(_) => {}
        }
    }
    assert_eq!(
        dec.next_frame().expect("next"),
        DecodeOutcome::Eof,
        "double EOF"
    );

    let mid = Rational::new_raw(1, 1);
    let DecodeOutcome::Frame(_) = dec.seek_exact(mid).expect("re-seek after drain") else {
        panic!("expected frame after seek post-EOF");
    };
}

// --- seek_exact ---

#[test]
fn seek_exact_t_zero() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let target = Rational::new_raw(0, 1);
    let DecodeOutcome::Frame(f) = dec.seek_exact(target).expect("seek") else {
        panic!("expected frame");
    };
    assert!(
        f.pts.ge(target),
        "pts {:?} for seek to 0",
        f.pts
    );
}

#[test]
fn seek_exact_two_seconds_h264() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let target = Rational::new_raw(2, 1);
    let DecodeOutcome::Frame(f) = dec.seek_exact(target).expect("seek") else {
        panic!("expected frame");
    };
    assert!(f.pts.ge(target), "pts {:?} should be >= {:?}", f.pts, target);
    assert!((f.pts.as_f64() - 2.0).abs() < 1e-4, "pts {:?}", f.pts);
}

#[test]
fn seek_exact_two_seconds_bframes() {
    let mut dec = Decoder::open(&asset("testsrc_bframes.mp4")).expect("open");
    let target = Rational::new_raw(2, 1);
    let DecodeOutcome::Frame(f) = dec.seek_exact(target).expect("seek") else {
        panic!("expected frame");
    };
    assert!(f.pts.ge(target));
    assert!((f.pts.as_f64() - 2.0).abs() < 1e-4);
}

#[test]
fn seek_exact_last_gop_then_monotonic_next_frames() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    // ~4.5s — still within last GOP before EOF
    let target = Rational::new_raw(9, 2);
    let DecodeOutcome::Frame(f0) = dec.seek_exact(target).expect("seek") else {
        panic!("expected frame");
    };
    assert!(f0.pts.ge(target));
    let DecodeOutcome::Frame(f1) = dec.next_frame().expect("next") else {
        panic!("expected next frame");
    };
    assert!(f1.pts.ge(f0.pts));
}

#[test]
fn seek_exact_then_next_advances_pts() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let t = Rational::new_raw(2, 1);
    let DecodeOutcome::Frame(f_seek) = dec.seek_exact(t).expect("seek") else {
        panic!("expected frame");
    };
    let DecodeOutcome::Frame(f_next) = dec.next_frame().expect("next") else {
        panic!("expected frame after seek");
    };
    assert!(
        f_next.pts.ge(f_seek.pts),
        "next {:?} should be >= seek {:?}",
        f_next.pts,
        f_seek.pts
    );
}

#[test]
fn seek_past_eof_yields_eof() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let far = Rational::new_raw(1_000_000, 1);
    let out = dec.seek_exact(far).expect("seek");
    assert_eq!(out, DecodeOutcome::Eof);
}

#[test]
fn chained_seeks_exact() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    for t in [
        Rational::new_raw(3, 1),
        Rational::new_raw(1, 1),
        Rational::new_raw(4, 1),
    ] {
        let DecodeOutcome::Frame(f) = dec.seek_exact(t).expect("seek") else {
            panic!("expected frame at {t}");
        };
        assert!(f.pts.ge(t), "pts {:?} should be >= {:?}", f.pts, t);
    }
}

// --- seek_scrub ---

#[test]
fn seek_scrub_mid_gop_snap() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let target = Rational::new_raw(5, 2);
    let DecodeOutcome::Frame(f) = dec.seek_scrub(target).expect("scrub") else {
        panic!("expected frame");
    };
    let two = Rational::new_raw(2, 1);
    assert!(
        f.pts.ge(two) && !f.pts.ge(Rational::new_raw(3, 1)),
        "expected snap near 2s, got {:?}",
        f.pts
    );
}

#[test]
fn seek_scrub_bframes_file() {
    let mut dec = Decoder::open(&asset("testsrc_bframes.mp4")).expect("open");
    let target = Rational::new_raw(5, 2);
    let DecodeOutcome::Frame(f) = dec.seek_scrub(target).expect("scrub") else {
        panic!("expected frame");
    };
    let two = Rational::new_raw(2, 1);
    assert!(f.pts.ge(two) && !f.pts.ge(Rational::new_raw(3, 1)), "{:?}", f.pts);
}

#[test]
fn seek_scrub_very_late_is_eof_or_last_keyframe_snap() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let out = dec
        .seek_scrub(Rational::new_raw(9_999_999, 1))
        .expect("scrub");
    match out {
        DecodeOutcome::Eof => {}
        DecodeOutcome::Frame(f) => {
            // Demuxer may clamp seek to end; scrub returns first picture after backward sync — often last GOP.
            assert!(
                f.pts.ge(Rational::new_raw(4, 1)),
                "late scrub should land in final second, pts {:?}",
                f.pts
            );
        }
    }
}

#[test]
fn seek_exact_rational_equivalent_to_two_seconds() {
    // 60/30 s == 2/1 s — same timestamp, should land same frame family
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(a) = dec.seek_exact(Rational::new_raw(60, 30)).expect("seek") else {
        panic!("expected frame");
    };
    let mut dec2 = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(b) = dec2.seek_exact(Rational::new_raw(2, 1)).expect("seek") else {
        panic!("expected frame");
    };
    assert_eq!(a.pts, b.pts);
}

#[test]
fn seek_exact_fractional_seconds() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let target = Rational::new_raw(10, 3); // ~3.333s
    let DecodeOutcome::Frame(f) = dec.seek_exact(target).expect("seek") else {
        panic!("expected frame");
    };
    assert!(f.pts.ge(target));
}

// --- Extra fixture-backed scenarios (engine / scrubbing edge cases) ---

#[test]
fn seek_scrub_t_zero_returns_picture() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(f) = dec
        .seek_scrub(Rational::new_raw(0, 1))
        .expect("scrub zero")
    else {
        panic!("expected frame at scrub 0");
    };
    let z = Rational::new_raw(0, 1);
    assert!(f.pts.ge(z));
}

#[test]
fn seek_exact_zero_matches_first_next_without_prior_seek() {
    let mut a = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(f_next) = a.next_frame().expect("next") else {
        panic!("frame");
    };
    let mut b = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(f_seek) = b.seek_exact(Rational::new_raw(0, 1)).expect("seek") else {
        panic!("frame");
    };
    assert_eq!(f_next.pts, f_seek.pts);
}

#[test]
fn info_timebase_positive_den_after_open_h264() {
    let dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    assert!(dec.info().timebase.den > 0);
}

#[test]
fn info_dimensions_stable_after_seek_exact() {
    let mut dec = Decoder::open(&asset("test_av.mp4")).expect("open");
    let w = dec.info().width;
    let h = dec.info().height;
    let _ = dec
        .seek_exact(Rational::new_raw(1, 1))
        .expect("seek");
    assert_eq!(dec.info().width, w);
    assert_eq!(dec.info().height, h);
}

#[test]
fn reopen_same_fixture_twice_has_same_geometry() {
    let d1 = Decoder::open(&asset("testsrc_bframes.mp4")).expect("open");
    let d2 = Decoder::open(&asset("testsrc_bframes.mp4")).expect("open");
    assert_eq!(d1.info().width, d2.info().width);
    assert_eq!(d1.info().height, d2.info().height);
}

#[test]
fn seek_exact_last_second_h264_still_decodes() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(f) =
        dec.seek_exact(Rational::new_raw(49, 10)).expect("seek") else {
        panic!("expected frame near 4.9s");
    };
    assert!(f.pts.ge(Rational::new_raw(49, 10)));
}

#[test]
fn seek_scrub_then_exact_same_decoder_session() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(_) = dec
        .seek_scrub(Rational::new_raw(1, 1))
        .expect("scrub")
    else {
        panic!("scrub");
    };
    let DecodeOutcome::Frame(f) = dec
        .seek_exact(Rational::new_raw(25, 10))
        .expect("exact")
    else {
        panic!("exact");
    };
    assert!(f.pts.ge(Rational::new_raw(25, 10)));
}

#[test]
fn drain_then_seek_scrub_recovers_frames() {
    let mut dec = Decoder::open(&asset("test_av.mp4")).expect("open");
    loop {
        match dec.next_frame().expect("next") {
            DecodeOutcome::Eof => break,
            DecodeOutcome::Frame(_) => {}
        }
    }
    let DecodeOutcome::Frame(f) = dec
        .seek_scrub(Rational::new_raw(1, 2))
        .expect("scrub after eof")
    else {
        panic!("expected scrub frame after full drain");
    };
    assert!(
        f.pts.ge(Rational::new_raw(0, 1)),
        "pts {:?} should be >= 0",
        f.pts
    );
}

#[test]
fn next_after_seek_exact_advances_from_fixture_start_region() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let DecodeOutcome::Frame(f0) =
        dec.seek_exact(Rational::new_raw(15, 10)).expect("seek") else {
        panic!("frame");
    };
    let DecodeOutcome::Frame(f1) = dec.next_frame().expect("next") else {
        panic!("next frame");
    };
    assert!(f1.pts.ge(f0.pts));
}

#[test]
fn mixed_av_seek_exact_mid_file_monotonic_stride() {
    let mut dec = Decoder::open(&asset("test_av.mp4")).expect("open");
    let DecodeOutcome::Frame(f) =
        dec.seek_exact(Rational::new_raw(1, 1)).expect("seek") else {
        panic!("frame");
    };
    assert_yuv420p_layout(&f, 128, 96);
}

#[test]
fn bframes_fixture_seek_exact_one_second_layout() {
    let mut dec = Decoder::open(&asset("testsrc_bframes.mp4")).expect("open");
    let DecodeOutcome::Frame(f) =
        dec.seek_exact(Rational::new_raw(1, 1)).expect("seek") else {
        panic!("frame");
    };
    assert_yuv420p_layout(&f, dec.info().width, dec.info().height);
}

