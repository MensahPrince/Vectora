//! Decoder benchmark over a real media file.
//!
//! Measures the decode paths that matter for preview and scrubbing, on the
//! platform-native backend:
//!
//! - probe and cold-open latency (media pool / first paint);
//! - sequential `next_frame` throughput, CPU and (where supported) zero-copy
//!   GPU output;
//! - playback via `frame_at` (the roll-forward path) vs the seek-per-frame
//!   baseline it replaces;
//! - random-access scrubbing (`frame_at` with uniformly random targets).
//!
//! Run:
//!   `cargo run --release -p cutlass-decoder --example decode_bench -- <media> [seconds]`
//!
//! `seconds` caps how much of the clip each phase decodes (default 10).

use std::path::{Path, PathBuf};
use std::time::Instant;

use cutlass_core::{Rational, RationalTime};
use cutlass_decoder::{OutputMode, open_video_decoder, probe};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: decode_bench <media> [seconds]");
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);

    let info = probe(&path).expect("probe failed");
    let fps = info.frame_rate;
    let frames = ((fps.as_f64() * seconds as f64).round() as i64)
        .min(info.frame_count.max(1))
        .max(1);

    println!("media: {}", path.display());
    println!(
        "  {}x{} @ {:.3} fps, {} frames total, audio: {}",
        info.width,
        info.height,
        fps.as_f64(),
        info.frame_count,
        if info.has_audio { "yes" } else { "no" },
    );
    println!("  benching {frames} frames ({seconds}s cap)\n");

    bench_probe(&path);
    bench_cold_open(&path);
    println!();

    let seq_cpu = bench_sequential(
        &path,
        OutputMode::Cpu,
        "sequential next_frame (cpu)",
        frames,
    );
    let _ = bench_sequential(
        &path,
        OutputMode::Gpu,
        "sequential next_frame (gpu)",
        frames,
    );
    let roll = bench_playback_roll(&path, fps, frames);
    let seek = bench_seek_per_frame(&path, fps, frames);
    let _ = bench_random_scrub(&path, fps, info.frame_count.max(1), 64);

    println!();
    if let (Some(roll), Some(seek)) = (roll, seek) {
        println!(
            "roll-forward speedup vs seek-per-frame: {:.1}x mean, {:.1}x p95",
            seek.mean / roll.mean,
            seek.p95 / roll.p95,
        );
    }
    if let (Some(seq), Some(roll)) = (seq_cpu, roll) {
        println!(
            "frame_at overhead vs raw sequential decode: {:.2}x mean",
            roll.mean / seq.mean
        );
    }
}

/// Probe latency: what the media pool pays to register a clip (open + info +
/// audio-track check, no frames decoded).
fn bench_probe(path: &Path) {
    const ITERS: usize = 5;
    let start = Instant::now();
    for _ in 0..ITERS {
        let _ = probe(path).expect("probe");
    }
    let mean = start.elapsed().as_secs_f64() * 1000.0 / ITERS as f64;
    println!("probe (open + info + audio check): {mean:>8.2} ms  (mean of {ITERS})");
}

/// Cold open → first decoded frame: what a preview pays before first paint.
fn bench_cold_open(path: &Path) {
    for (mode, label) in [(OutputMode::Cpu, "cpu"), (OutputMode::Gpu, "gpu")] {
        const ITERS: usize = 5;
        let mut open_ms = 0.0;
        let mut first_ms = 0.0;
        let mut ok = true;
        for _ in 0..ITERS {
            let start = Instant::now();
            let mut dec = match open_video_decoder(path, mode) {
                Ok(dec) => dec,
                Err(e) => {
                    println!("cold open ({label}): unsupported ({e})");
                    ok = false;
                    break;
                }
            };
            open_ms += start.elapsed().as_secs_f64() * 1000.0;

            let start = Instant::now();
            match dec.next_frame() {
                Ok(Some(_)) => first_ms += start.elapsed().as_secs_f64() * 1000.0,
                Ok(None) => panic!("no first frame"),
                Err(e) => {
                    println!("cold open ({label}): first frame unsupported ({e})");
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            println!(
                "cold open -> first frame ({label}):  {:>8.2} ms open + {:>6.2} ms first frame",
                open_ms / ITERS as f64,
                first_ms / ITERS as f64,
            );
        }
    }
}

/// Raw forward decode throughput in `mode`.
fn bench_sequential(path: &Path, mode: OutputMode, label: &str, frames: i64) -> Option<Stats> {
    let mut dec = match open_video_decoder(path, mode) {
        Ok(dec) => dec,
        Err(e) => {
            println!("{label}: skipped ({e})");
            return None;
        }
    };
    let mut samples = Vec::with_capacity(frames as usize);
    let total = Instant::now();
    for _ in 0..frames {
        let start = Instant::now();
        match dec.next_frame() {
            Ok(Some(frame)) => {
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
                drop(frame);
            }
            Ok(None) => break,
            Err(e) => {
                println!("{label}: skipped ({e})");
                return None;
            }
        }
    }
    let stats = Stats::from_samples(samples, total.elapsed().as_secs_f64());
    stats.print(label);
    Some(stats)
}

/// Playback simulation: one `frame_at` per native frame tick, in order — the
/// preview worker's pattern. Exercises the roll-forward fast path.
fn bench_playback_roll(path: &Path, fps: Rational, frames: i64) -> Option<Stats> {
    let mut dec = open_video_decoder(path, OutputMode::Cpu).ok()?;
    let mut samples = Vec::with_capacity(frames as usize);
    let total = Instant::now();
    for i in 0..frames {
        let target = RationalTime::new(i, fps);
        let start = Instant::now();
        match dec.frame_at(target) {
            Ok(Some(_)) => samples.push(start.elapsed().as_secs_f64() * 1000.0),
            Ok(None) => break,
            Err(e) => panic!("frame_at failed at {i}: {e}"),
        }
    }
    let stats = Stats::from_samples(samples, total.elapsed().as_secs_f64());
    stats.print("playback frame_at (roll-fwd)");
    Some(stats)
}

/// The old path `frame_at` replaces: an explicit seek + forward walk for every
/// target, which re-decodes the GOP prefix each time.
fn bench_seek_per_frame(path: &Path, fps: Rational, frames: i64) -> Option<Stats> {
    let mut dec = open_video_decoder(path, OutputMode::Cpu).ok()?;
    let mut samples = Vec::with_capacity(frames as usize);
    let total = Instant::now();
    'targets: for i in 0..frames {
        let target = RationalTime::new(i, fps);
        let start = Instant::now();
        dec.seek(target).expect("seek");
        loop {
            match dec.next_frame() {
                Ok(Some(frame)) => {
                    if frame.pts.compare(target) != core::cmp::Ordering::Less {
                        samples.push(start.elapsed().as_secs_f64() * 1000.0);
                        break;
                    }
                }
                Ok(None) => break 'targets,
                Err(e) => panic!("decode failed at {i}: {e}"),
            }
        }
    }
    let stats = Stats::from_samples(samples, total.elapsed().as_secs_f64());
    stats.print("playback seek-per-frame (old)");
    Some(stats)
}

/// Random-access scrubbing: `frame_at` with uniformly random targets across
/// the whole clip (mostly real seeks; occasionally a lucky roll).
fn bench_random_scrub(
    path: &Path,
    fps: Rational,
    total_frames: i64,
    jumps: usize,
) -> Option<Stats> {
    let mut dec = open_video_decoder(path, OutputMode::Cpu).ok()?;
    // Deterministic LCG so runs are comparable; no rand dependency.
    let mut state: u64 = 0x243F_6A88_85A3_08D3;
    let mut samples = Vec::with_capacity(jumps);
    let total = Instant::now();
    for _ in 0..jumps {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let target = RationalTime::new(((state >> 33) % total_frames as u64) as i64, fps);
        let start = Instant::now();
        match dec.frame_at(target) {
            Ok(Some(_)) => samples.push(start.elapsed().as_secs_f64() * 1000.0),
            Ok(None) => continue, // duration overshoot near EOF: skip sample
            Err(e) => panic!("scrub frame_at failed: {e}"),
        }
    }
    let stats = Stats::from_samples(samples, total.elapsed().as_secs_f64());
    stats.print("random scrub frame_at (x64)");
    Some(stats)
}

#[derive(Clone, Copy)]
struct Stats {
    mean: f64,
    p50: f64,
    p95: f64,
    max: f64,
    count: usize,
    eff_fps: f64,
}

impl Stats {
    fn from_samples(mut samples_ms: Vec<f64>, total_s: f64) -> Self {
        samples_ms.sort_by(|a, b| a.total_cmp(b));
        let n = samples_ms.len().max(1);
        Self {
            mean: samples_ms.iter().sum::<f64>() / n as f64,
            p50: samples_ms.get(n / 2).copied().unwrap_or(0.0),
            p95: samples_ms
                .get((n * 95 / 100).min(n - 1))
                .copied()
                .unwrap_or(0.0),
            max: samples_ms.last().copied().unwrap_or(0.0),
            count: samples_ms.len(),
            eff_fps: if total_s > 0.0 {
                samples_ms.len() as f64 / total_s
            } else {
                0.0
            },
        }
    }

    fn print(&self, label: &str) {
        println!(
            "{label:<30} {:>5} frames | {:>7.2} ms mean | {:>7.2} ms p50 | {:>7.2} ms p95 | {:>8.2} ms max | {:>7.1} eff fps",
            self.count, self.mean, self.p50, self.p95, self.max, self.eff_fps,
        );
    }
}
