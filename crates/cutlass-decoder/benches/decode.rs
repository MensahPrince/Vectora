//! Sequential video decode throughput: software vs hardware (the preview
//! playback hot path). Each iteration seeks to the start and decodes a run of
//! frames, mirroring what a play press does — pull frame after frame off one
//! reused decoder. Throughput is reported in frames, so criterion's
//! `elem/s` reads directly as decode FPS: anything below the file's own frame
//! rate is a guaranteed playback hitch.
//!
//! Run:
//!   `cargo bench -p cutlass-decoder --bench decode`
//!   `CUTLASS_BENCH_ASSET=local-assets/assets/untitled.mp4 cargo bench -p cutlass-decoder --bench decode`
//!   `CUTLASS_BENCH_FRAMES=120 cargo bench -p cutlass-decoder --bench decode`

use std::path::PathBuf;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use cutlass_decoder::{DecodeOptions, Decoder, HwAccel};

fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local-assets/assets")
}

fn bench_asset() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CUTLASS_BENCH_ASSET") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let dir = assets_dir();
    for name in [
        "untitled.mp4",
        "15531444_1920_1080_24fps.mp4",
        "6137050-hd_1920_1080_24fps.mp4",
    ] {
        let path = dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "mp4"))
}

fn frame_count() -> u64 {
    std::env::var("CUTLASS_BENCH_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60)
}

/// Decode `frames` sequential frames from the start, reusing `decoder` the way
/// the preview decoder pool does. Returns how many actually came back (a short
/// clip may have fewer).
fn decode_run(decoder: &mut Decoder, frames: u64) -> u64 {
    decoder.seek_ticks(0).expect("seek to start");
    let mut decoded = 0;
    for _ in 0..frames {
        match decoder.next_frame().expect("decode") {
            Some(_) => decoded += 1,
            None => break,
        }
    }
    decoded
}

fn bench_decode(c: &mut Criterion) {
    let Some(path) = bench_asset() else {
        eprintln!(
            "decode bench: no CUTLASS_BENCH_ASSET or local-assets/assets/*.mp4, skipping decode cases"
        );
        return;
    };
    let frames = frame_count();

    let mut group = c.benchmark_group("decode/sequential");
    group.throughput(Throughput::Elements(frames));
    group.sample_size(10);

    // Software decode: exactly what the preview DecoderPool forces today
    // (`DecodeOptions::default().hw_accel(HwAccel::None)`).
    {
        let mut decoder = Decoder::open_with(&path, DecodeOptions::default().hw_accel(HwAccel::None))
            .expect("open software decoder");
        let info = decoder.info();
        eprintln!(
            "decode bench: {} — {}x{} @ {:?} fps, software ({})",
            path.display(),
            info.width,
            info.height,
            info.frame_rate,
            info.hw_accel.name(),
        );
        group.bench_function("software", |b| {
            b.iter(|| decode_run(&mut decoder, frames));
        });
    }

    // Hardware decode (Auto → VideoToolbox on macOS). Skip the case when the
    // platform falls back to software so the label never lies.
    {
        let mut decoder = Decoder::open_with(&path, DecodeOptions::default().hw_accel(HwAccel::Auto))
            .expect("open hardware decoder");
        let active = decoder.info().hw_accel;
        if active.uses_hardware() {
            eprintln!("decode bench: hardware backend active = {}", active.name());
            group.bench_function("hardware", |b| {
                b.iter(|| decode_run(&mut decoder, frames));
            });
        } else {
            eprintln!("decode bench: no hardware backend available, skipping hardware case");
        }
    }

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
