//! GPU + real media tests (fixtures under `decoder/tests/assets`).

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use decoder::{CpuFrame, DecodeOutcome, Decoder, DecodedVideoFrame, FrameData, Plane, PixelFormat, Rational};
use engine::{Engine, EngineEvent, EventReceiver, RequestId, SourceId};
use renderer::{upload_decoded_frame_for_test, Layer, RenderTarget, Renderer, Transform};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../decoder/tests/assets")
        .join(name)
}

fn recv_opened(rx: &EventReceiver, expect_rid: RequestId) -> SourceId {
    match rx.recv_timeout(Duration::from_secs(5)).expect("Opened") {
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

fn open_engine_handshake(engine: &Engine, rx: &EventReceiver, name: &str) -> SourceId {
    let (sid, rid) = engine.open(asset(name));
    assert_eq!(recv_opened(rx, rid), sid);
    sid
}

fn synthetic_nv12(y: u8, u: u8, v: u8, w: u32, h: u32) -> DecodedVideoFrame {
    assert!(w >= 2 && h >= 2 && w.is_multiple_of(2) && h.is_multiple_of(2));
    let y_sz = (w * h) as usize;
    let uv_rows = (h / 2) as usize;
    let uv_stride = w as usize;
    let mut uv = vec![0u8; uv_rows * uv_stride];
    for row in 0..uv_rows {
        for col in 0..(w / 2) as usize {
            let i = row * uv_stride + col * 2;
            uv[i] = u;
            uv[i + 1] = v;
        }
    }
    DecodedVideoFrame {
        width: w,
        height: h,
        pts: Rational::new_raw(0, 1),
        timebase: Rational::new_raw(1, 30_000),
        data: FrameData::Cpu(CpuFrame {
            format: PixelFormat::Nv12,
            planes: vec![
                Plane {
                    data: vec![y; y_sz],
                    stride: w as usize,
                },
                Plane {
                    data: uv,
                    stride: uv_stride,
                },
            ],
        }),
    }
}

fn pixel_variance_rgba(buf: &[u8]) -> f64 {
    let n = buf.len();
    let sum: u64 = buf.iter().map(|&b| u64::from(b)).sum();
    let mean = sum as f64 / n as f64;
    buf.iter()
        .map(|&b| {
            let d = f64::from(b) - mean;
            d * d
        })
        .sum::<f64>()
        / n as f64
}

fn rgba_fingerprint(buf: &[u8]) -> u64 {
    buf.iter()
        .fold(5381u64, |h, &b| h.wrapping_mul(33).wrapping_add(u64::from(b)))
}

#[test]
fn upload_yuv420p_320x240_succeeds_and_textures_have_expected_size() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let frame = match dec.next_frame().expect("decode") {
        DecodeOutcome::Frame(f) => f,
        DecodeOutcome::Eof => panic!("eof"),
    };
    let r = Renderer::new().expect("renderer");
    let tex = upload_decoded_frame_for_test(&r, &frame).expect("upload");
    assert_eq!(tex.len(), 3);
    assert_eq!(tex[0].size().width, 320);
    assert_eq!(tex[0].size().height, 240);
    assert_eq!(tex[1].size().width, 160);
    assert_eq!(tex[1].size().height, 120);
    assert_eq!(tex[2].size().width, 160);
    assert_eq!(tex[2].size().height, 120);
}

#[test]
fn render_yuv420p_real_h264_first_frame_produces_non_zero_rgb() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let frame = match dec.next_frame().expect("decode") {
        DecodeOutcome::Frame(f) => f,
        DecodeOutcome::Eof => panic!("eof"),
    };
    let mut r = Renderer::new().expect("renderer");
    let target = RenderTarget::new(r.device(), frame.width, frame.height);
    r.render(
        &[Layer {
            frame,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &target,
    )
    .expect("render");
    let px = r.read_pixels_rgba8(&target).expect("read");
    let v = pixel_variance_rgba(&px);
    assert!(v > 50.0, "expected visible variance, got {v}");
}

#[test]
fn render_nv12_solid_colors_match_yuv420p_solid_colors() {
    let mut r = Renderer::new().expect("renderer");
    let w = 32u32;
    let h = 32u32;
    let yuv = DecodedVideoFrame {
        width: w,
        height: h,
        pts: Rational::new_raw(0, 1),
        timebase: Rational::new_raw(1, 30),
        data: FrameData::Cpu(CpuFrame {
            format: PixelFormat::Yuv420p,
            planes: vec![
                Plane {
                    data: vec![125u8; (w * h) as usize],
                    stride: w as usize,
                },
                Plane {
                    data: vec![128u8; (w / 2 * h / 2) as usize],
                    stride: (w / 2) as usize,
                },
                Plane {
                    data: vec![128u8; (w / 2 * h / 2) as usize],
                    stride: (w / 2) as usize,
                },
            ],
        }),
    };
    let nv = synthetic_nv12(125, 128, 128, w, h);
    let t1 = RenderTarget::new(r.device(), w, h);
    let t2 = RenderTarget::new(r.device(), w, h);
    r.render(
        &[Layer {
            frame: yuv,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &t1,
    )
    .expect("yuv");
    r.render(
        &[Layer {
            frame: nv,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &t2,
    )
    .expect("nv12");
    let a = r.read_pixels_rgba8(&t1).expect("r1");
    let b = r.read_pixels_rgba8(&t2).expect("r2");
    assert_eq!(a.len(), b.len());
    for i in (0..a.len()).step_by(4) {
        for c in 0..3 {
            let da = a[i + c] as i16;
            let db = b[i + c] as i16;
            assert!((da - db).abs() <= 2, "mismatch at {i}+{c}: {} vs {}", a[i + c], b[i + c]);
        }
    }
}

#[test]
fn upload_nv12_synthetic_plane_sizes_match_320p() {
    let frame = synthetic_nv12(40, 120, 130, 320, 240);
    let r = Renderer::new().expect("renderer");
    let tex = upload_decoded_frame_for_test(&r, &frame).expect("upload");
    assert_eq!(tex.len(), 2);
    assert_eq!(tex[0].size().width, 320);
    assert_eq!(tex[0].size().height, 240);
    assert_eq!(tex[1].size().width, 160);
    assert_eq!(tex[1].size().height, 120);
}

#[test]
fn render_nv12_synthetic_pattern_has_variance() {
    let mut r = Renderer::new().expect("renderer");
    let w = 64u32;
    let h = 64u32;
    let mut y_plane = vec![0u8; (w * h) as usize];
    for row in 0..h {
        for col in 0..w {
            y_plane[(row * w + col) as usize] = ((row.wrapping_add(col)) & 0xff) as u8;
        }
    }
    let uv_rows = (h / 2) as usize;
    let uv_stride = w as usize;
    let mut uv = vec![120u8; uv_rows * uv_stride];
    for row in 0..uv_rows {
        for col in 0..(w / 2) as usize {
            let i = row * uv_stride + col * 2;
            uv[i] = (col * 4) as u8;
            uv[i + 1] = (row * 4) as u8;
        }
    }
    let frame = DecodedVideoFrame {
        width: w,
        height: h,
        pts: Rational::new_raw(0, 1),
        timebase: Rational::new_raw(1, 30),
        data: FrameData::Cpu(CpuFrame {
            format: PixelFormat::Nv12,
            planes: vec![
                Plane {
                    data: y_plane,
                    stride: w as usize,
                },
                Plane {
                    data: uv,
                    stride: uv_stride,
                },
            ],
        }),
    };
    let target = RenderTarget::new(r.device(), w, h);
    r.render(
        &[Layer {
            frame,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &target,
    )
    .expect("render");
    let px = r.read_pixels_rgba8(&target).expect("read");
    assert!(
        pixel_variance_rgba(&px) > 100.0,
        "expected patterned NV12 frame to vary in RGB"
    );
}

#[test]
fn render_rgba8_passes_through_unchanged() {
    let w = 4u32;
    let h = 3u32;
    let mut data = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            let xb = x as u8;
            data.extend_from_slice(&[xb, xb.wrapping_mul(2), xb.wrapping_mul(3), 255]);
        }
    }
    let frame = DecodedVideoFrame {
        width: w,
        height: h,
        pts: Rational::new_raw(0, 1),
        timebase: Rational::new_raw(1, 1),
        data: FrameData::Cpu(CpuFrame {
            format: PixelFormat::Rgba8,
            planes: vec![Plane {
                stride: (w * 4) as usize,
                data,
            }],
        }),
    };
    let mut r = Renderer::new().expect("renderer");
    let target = RenderTarget::new(r.device(), w, h);
    r.render(
        &[Layer {
            frame: frame.clone(),
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &target,
    )
    .expect("render");
    let gpu = r.read_pixels_rgba8(&target).expect("read");
    let cpu = match &frame.data {
        FrameData::Cpu(c) => &c.planes[0].data,
        _ => panic!(),
    };
    assert_eq!(gpu.as_slice(), cpu.as_slice());
}

#[test]
fn engine_to_renderer_h264_pipeline_produces_pixels() {
    let (engine, rx) = Engine::new();
    let sid = open_engine_handshake(&engine, &rx, "testsrc_h264.mp4");
    let req = engine.seek_exact(sid, Rational::new_raw(2, 1));
    let frame = loop {
        match rx.recv_timeout(Duration::from_secs(5)).expect("ev") {
            EngineEvent::Frame {
                frame,
                request_id: Some(r),
                ..
            } if r == req => break frame,
            EngineEvent::Error { error, .. } => panic!("{error}"),
            _ => {}
        }
    };
    let mut r = Renderer::new().expect("renderer");
    let target = RenderTarget::new(r.device(), frame.width, frame.height);
    r.render(
        &[Layer {
            frame,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &target,
    )
    .expect("render");
    assert!(pixel_variance_rgba(&r.read_pixels_rgba8(&target).expect("px")) > 10.0);
    engine.close(sid);
    drain_closed(&rx, sid);
}

#[test]
fn engine_to_renderer_bframes_pipeline_produces_pixels() {
    let (engine, rx) = Engine::new();
    let sid = open_engine_handshake(&engine, &rx, "testsrc_bframes.mp4");
    let req = engine.seek_exact(sid, Rational::new_raw(2, 1));
    let frame = loop {
        match rx.recv_timeout(Duration::from_secs(5)).expect("ev") {
            EngineEvent::Frame {
                frame,
                request_id: Some(r),
                ..
            } if r == req => break frame,
            EngineEvent::Error { error, .. } => panic!("{error}"),
            _ => {}
        }
    };
    let mut r = Renderer::new().expect("renderer");
    let target = RenderTarget::new(r.device(), frame.width, frame.height);
    r.render(
        &[Layer {
            frame,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &target,
    )
    .expect("render");
    assert!(pixel_variance_rgba(&r.read_pixels_rgba8(&target).expect("px")) > 10.0);
    engine.close(sid);
    drain_closed(&rx, sid);
}

#[test]
fn decode_three_consecutive_h264_frames_each_renders_320x240_with_variance() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let mut r = Renderer::new().expect("renderer");
    for i in 0..3 {
        let frame = match dec.next_frame().expect("decode") {
            DecodeOutcome::Frame(f) => f,
            DecodeOutcome::Eof => panic!("unexpected eof at frame {i}"),
        };
        assert_eq!((frame.width, frame.height), (320, 240));
        let target = RenderTarget::new(r.device(), frame.width, frame.height);
        r.render(
            &[Layer {
                frame,
                transform: Transform::identity(),
                opacity: 1.0,
            }],
            &target,
        )
        .expect("render");
        let v = pixel_variance_rgba(&r.read_pixels_rgba8(&target).expect("read"));
        assert!(v > 30.0, "frame {i} should have textured content, var={v}");
    }
}

#[test]
fn h264_first_frame_and_seek_two_seconds_yield_different_rgba_fingerprints() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let f0 = match dec.next_frame().expect("d0") {
        DecodeOutcome::Frame(f) => f,
        DecodeOutcome::Eof => panic!("eof"),
    };
    let mut r = Renderer::new().expect("renderer");
    let render_fp =
        |r: &mut Renderer, frame: DecodedVideoFrame| -> u64 {
            let target = RenderTarget::new(r.device(), frame.width, frame.height);
            r.render(
                &[Layer {
                    frame,
                    transform: Transform::identity(),
                    opacity: 1.0,
                }],
                &target,
            )
            .expect("render");
            rgba_fingerprint(&r.read_pixels_rgba8(&target).expect("read"))
        };
    let fp0 = render_fp(&mut r, f0);
    dec.seek_exact(Rational::new_raw(2, 1)).expect("seek");
    let f2 = match dec.next_frame().expect("d2") {
        DecodeOutcome::Frame(f) => f,
        DecodeOutcome::Eof => panic!("eof after seek"),
    };
    let fp2 = render_fp(&mut r, f2);
    assert_ne!(
        fp0, fp2,
        "different timeline positions should produce different RGBA fingerprints"
    );
}

#[test]
fn engine_test_av_mp4_frame_renders_at_128x96() {
    let (engine, rx) = Engine::new();
    let sid = open_engine_handshake(&engine, &rx, "test_av.mp4");
    let req = engine.next_frame(sid);
    let frame = loop {
        match rx.recv_timeout(Duration::from_secs(5)).expect("ev") {
            EngineEvent::Frame {
                frame,
                request_id: Some(r),
                ..
            } if r == req => break frame,
            EngineEvent::Error { error, .. } => panic!("{error}"),
            _ => {}
        }
    };
    assert_eq!((frame.width, frame.height), (128, 96));
    let mut r = Renderer::new().expect("renderer");
    let target = RenderTarget::new(r.device(), frame.width, frame.height);
    r.render(
        &[Layer {
            frame,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &target,
    )
    .expect("render");
    assert!(pixel_variance_rgba(&r.read_pixels_rgba8(&target).expect("px")) > 5.0);
    engine.close(sid);
    drain_closed(&rx, sid);
}

#[test]
fn reuse_single_render_target_for_two_decoded_h264_frames() {
    let mut dec = Decoder::open(&asset("testsrc_h264.mp4")).expect("open");
    let f0 = match dec.next_frame().expect("a") {
        DecodeOutcome::Frame(f) => f,
        DecodeOutcome::Eof => panic!("eof"),
    };
    let f1 = match dec.next_frame().expect("b") {
        DecodeOutcome::Frame(f) => f,
        DecodeOutcome::Eof => panic!("eof"),
    };
    let mut r = Renderer::new().expect("renderer");
    let target = RenderTarget::new(r.device(), 320, 240);
    r.render(
        &[Layer {
            frame: f0,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &target,
    )
    .expect("r0");
    let a = rgba_fingerprint(&r.read_pixels_rgba8(&target).expect("p0"));
    r.render(
        &[Layer {
            frame: f1,
            transform: Transform::identity(),
            opacity: 1.0,
        }],
        &target,
    )
    .expect("r1");
    let b = rgba_fingerprint(&r.read_pixels_rgba8(&target).expect("p1"));
    assert_ne!(a, b, "adjacent decoded frames should change the RGBA buffer");
}

fn drain_closed(rx: &EventReceiver, sid: SourceId) {
    loop {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(EngineEvent::Closed { source_id }) if source_id == sid => break,
            Ok(other) => eprintln!("note: extra event {other:?}"),
            Err(_) => break,
        }
    }
}
