//! Shared helpers for the integration test binaries (`decode_integration`,
//! `probe_integration`). Each integration test is its own crate, so an unused
//! helper here is dead code in the binaries that don't reference it — silence
//! that with a crate-level allow rather than per-item attributes.

#![allow(dead_code)]

use std::path::PathBuf;

use decoder::{DecodedVideoFrame, FrameData, PixelFormat};

pub fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("assets")
        .join(name)
}

/// Assert a YUV420P frame has three planes with consistent stride × height sizing.
pub fn assert_yuv420p_layout(f: &DecodedVideoFrame, width: u32, height: u32) {
    assert_eq!(f.width, width);
    assert_eq!(f.height, height);
    let FrameData::Cpu(cpu) = &f.data else {
        panic!("expected FrameData::Cpu");
    };
    assert_eq!(cpu.format, PixelFormat::Yuv420p);
    assert_eq!(cpu.planes.len(), 3);
    let h = height as usize;
    let h2 = (height / 2) as usize;
    assert_eq!(cpu.planes[0].data.len(), cpu.planes[0].stride * h);
    assert_eq!(cpu.planes[1].data.len(), cpu.planes[1].stride * h2);
    assert_eq!(cpu.planes[2].data.len(), cpu.planes[2].stride * h2);
}
