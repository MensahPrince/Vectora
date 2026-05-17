//! Integration tests: real FFmpeg I/O via `tests/assets/` (symlinks to decoder fixtures; regenerate there or via `tests/assets/regenerate.sh`).

#![cfg(unix)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use decoder::{DecoderError, PixelFormat, Rational};
use engine::{Engine, EngineError, EngineEvent, EventReceiver, RequestId, SourceId};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("assets")
        .join(name)
}

fn recv_opened(rx: &EventReceiver, expect_rid: RequestId) -> SourceId {
    match rx.recv_timeout(Duration::from_secs(5)).expect("recv Opened") {
        EngineEvent::Opened {
            source_id,
            request_id,
            ..
        } => {
            assert_eq!(request_id, expect_rid);
            source_id
        }
        other => panic!("expected Opened, got {other:?}"),
    }
}

#[test]
fn engine_opens_real_file() {
    let (engine, rx) = Engine::new();
    let (sid, rid) = engine.open(asset("testsrc_h264.mp4"));
    match rx.recv_timeout(Duration::from_secs(5)).expect("event") {
        EngineEvent::Opened {
            source_id,
            info,
            request_id,
        } => {
            assert_eq!(source_id, sid);
            assert_eq!(request_id, rid);
            assert_eq!(info.width, 320);
            assert_eq!(info.height, 240);
            assert_eq!(info.pixel_format, PixelFormat::Yuv420p);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn engine_open_missing_file_emits_error() {
    let (engine, rx) = Engine::new();
    let bad = PathBuf::from("/no/such/cutlass_engine_missing.mp4");
    let (_sid, rid) = engine.open(bad);
    match rx.recv_timeout(Duration::from_secs(5)).expect("event") {
        EngineEvent::Error {
            source_id: Some(_),
            error: EngineError::Decoder(DecoderError::Open(_)),
            request_id: Some(r),
        } => assert_eq!(r, rid),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn engine_open_audio_only_emits_unsupported() {
    let (engine, rx) = Engine::new();
    let (_sid, _rid) = engine.open(asset("audio_only.m4a"));
    match rx.recv_timeout(Duration::from_secs(5)).expect("event") {
        EngineEvent::Error {
            error: EngineError::Decoder(DecoderError::Unsupported { what }),
            ..
        } => assert!(
            what.contains("no video stream"),
            "unexpected message: {what}"
        ),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn engine_drop_disconnects_event_receiver() {
    let (engine, rx) = Engine::new();
    drop(engine);
    match rx.recv_timeout(Duration::from_secs(3)) {
        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {}
        Ok(ev) => panic!("unexpected event after drop: {ev:?}"),
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
            panic!("expected disconnected receiver after engine drop")
        }
    }
}

#[test]
fn engine_rejects_second_open_until_close() {
    let (engine, rx) = Engine::new();
    let (sid1, rid1) = engine.open(asset("testsrc_h264.mp4"));
    recv_opened(&rx, rid1);

    let (_sid2, rid2) = engine.open(asset("testsrc_h264.mp4"));
    match rx.recv_timeout(Duration::from_secs(5)).expect("event") {
        EngineEvent::Error {
            error: EngineError::SourceAlreadyOpen(id),
            request_id: Some(r),
            ..
        } => {
            assert_eq!(id, sid1);
            assert_eq!(r, rid2);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn engine_seek_exact_to_two_seconds() {
    let (engine, rx) = Engine::new();
    let (sid, open_rid) = engine.open(asset("testsrc_h264.mp4"));
    recv_opened(&rx, open_rid);

    let target = Rational::new_raw(2, 1);
    let req = engine.seek_exact(sid, target);
    match rx.recv_timeout(Duration::from_secs(5)).expect("frame") {
        EngineEvent::Frame {
            source_id,
            frame,
            request_id: Some(r),
        } => {
            assert_eq!(source_id, sid);
            assert_eq!(r, req);
            assert!(frame.pts.ge(target), "pts {:?}", frame.pts);
            assert!((frame.pts.as_f64() - 2.0).abs() < 1e-3);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn engine_seek_exact_bframes() {
    let (engine, rx) = Engine::new();
    let (sid, open_rid) = engine.open(asset("testsrc_bframes.mp4"));
    recv_opened(&rx, open_rid);

    let target = Rational::new_raw(2, 1);
    let req = engine.seek_exact(sid, target);
    match rx.recv_timeout(Duration::from_secs(5)).expect("frame") {
        EngineEvent::Frame {
            frame,
            request_id: Some(r),
            ..
        } => {
            assert_eq!(r, req);
            assert!(frame.pts.ge(target));
            assert!((frame.pts.as_f64() - 2.0).abs() < 1e-3);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn engine_next_frame_after_seek_monotonic() {
    let (engine, rx) = Engine::new();
    let (sid, open_rid) = engine.open(asset("testsrc_h264.mp4"));
    recv_opened(&rx, open_rid);

    let t = Rational::new_raw(2, 1);
    let _ = engine.seek_exact(sid, t);
    let mut last = match rx.recv_timeout(Duration::from_secs(5)).expect("seek frame") {
        EngineEvent::Frame { frame, .. } => frame.pts,
        other => panic!("unexpected {other:?}"),
    };

    for _ in 0..3 {
        engine.next_frame(sid);
        let pts = match rx.recv_timeout(Duration::from_secs(5)).expect("next") {
            EngineEvent::Frame { frame, .. } => frame.pts,
            other => panic!("unexpected {other:?}"),
        };
        assert!(
            pts.ge(last),
            "pts should increase: last {:?} next {:?}",
            last,
            pts
        );
        last = pts;
    }
}

#[test]
fn engine_close_releases_source() {
    let (engine, rx) = Engine::new();
    let (sid, open_rid) = engine.open(asset("testsrc_h264.mp4"));
    recv_opened(&rx, open_rid);

    engine.close(sid);
    match rx.recv_timeout(Duration::from_secs(5)).expect("closed") {
        EngineEvent::Closed { source_id } => assert_eq!(source_id, sid),
        other => panic!("unexpected {other:?}"),
    }

    engine.next_frame(sid);
    match rx.recv_timeout(Duration::from_secs(5)).expect("err") {
        EngineEvent::Error {
            error: EngineError::SourceNotFound(id),
            ..
        } => assert_eq!(id, sid),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn engine_seek_past_eof() {
    let (engine, rx) = Engine::new();
    let (sid, open_rid) = engine.open(asset("testsrc_h264.mp4"));
    recv_opened(&rx, open_rid);

    let far = Rational::new_raw(1_000_000, 1);
    let req = engine.seek_exact(sid, far);
    match rx.recv_timeout(Duration::from_secs(5)).expect("eof") {
        EngineEvent::Eof {
            source_id,
            request_id: Some(r),
        } => {
            assert_eq!(source_id, sid);
            assert_eq!(r, req);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn scrub_coalesces_burst() {
    let (engine, rx) = Engine::new();
    let (sid, open_rid) = engine.open(asset("testsrc_h264.mp4"));
    recv_opened(&rx, open_rid);

    for i in 1..=20 {
        engine.seek_scrub(sid, Rational::new_raw(i, 10));
    }

    let mut scrub_frames = 0u32;
    let mut last_pts = Rational::new_raw(0, 1);
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(EngineEvent::Frame {
                request_id: None,
                frame,
                ..
            }) => {
                scrub_frames += 1;
                last_pts = frame.pts;
            }
            Ok(EngineEvent::Error { .. }) => panic!("unexpected error during scrub burst"),
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    assert!(
        scrub_frames <= 20,
        "scrub should not decode more frames than upper bound: {scrub_frames}"
    );
    assert!(
        scrub_frames < 10,
        "bounded scrub signal + slot should coalesce a tight burst (got {scrub_frames} frames)"
    );

    let target = Rational::new_raw(20, 10);
    assert!(
        last_pts.ge(Rational::new_raw(15, 10)),
        "expected PTS toward end of burst (~2s), got {:?} (target {:?})",
        last_pts,
        target
    );
    assert!(
        !last_pts.ge(Rational::new_raw(5, 1)),
        "clip is ~5s; scrub snap should stay below duration (pts {:?})",
        last_pts
    );
}

#[test]
fn scrub_after_exact_does_not_lose_exact() {
    let (engine, rx) = Engine::new();
    let (sid, open_rid) = engine.open(asset("testsrc_h264.mp4"));
    recv_opened(&rx, open_rid);

    let two = Rational::new_raw(2, 1);
    let rid_ex = engine.seek_exact(sid, two);
    engine.seek_scrub(sid, Rational::new_raw(4, 1));

    let mut saw_exact = false;
    let mut saw_scrub = false;
    let deadline = Instant::now() + Duration::from_secs(8);
    while !(saw_exact && saw_scrub) && Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(2)).expect("recv") {
            EngineEvent::Frame {
                frame,
                request_id: Some(r),
                ..
            } if r == rid_ex => {
                assert!(frame.pts.ge(two), "exact seek {:?}", frame.pts);
                assert!((frame.pts.as_f64() - 2.0).abs() < 0.05, "{:?}", frame.pts);
                saw_exact = true;
            }
            EngineEvent::Frame {
                frame,
                request_id: None,
                ..
            } => {
                assert!(
                    frame.pts.ge(Rational::new_raw(35, 10)),
                    "scrub toward ~4s should land past ~3.5s, got {:?}",
                    frame.pts
                );
                saw_scrub = true;
            }
            EngineEvent::Error { error, .. } => panic!("unexpected error: {error}"),
            _ => {}
        }
    }

    assert!(saw_exact && saw_scrub, "expected exact frame then scrub frame");
}

#[test]
fn scrub_on_unopened_source() {
    let (engine, rx) = Engine::new();
    engine.seek_scrub(SourceId(1), Rational::new_raw(1, 1));
    match rx.recv_timeout(Duration::from_secs(3)).expect("event") {
        EngineEvent::Error {
            error: EngineError::SourceNotFound(id),
            request_id: None,
            ..
        } => assert_eq!(id, SourceId(1)),
        other => panic!("unexpected {other:?}"),
    }
}
