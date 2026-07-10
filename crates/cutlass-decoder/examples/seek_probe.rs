//! Diagnostic: probe the native decoder's random-access behavior against
//! long-GOP / open-GOP sources (the `export.mp4` "Cannot Decode" repro — see
//! REVIEW.md, 2026-07-10). Cold-seeks a spread of absolute times, then checks
//! sequential decode across a GOP boundary and a back-off-and-walk fallback.
//! The hardcoded times target a ~104 s clip with ~10 s GOPs; adjust for other
//! assets.
//!
//! Run: cargo run --release -p cutlass-decoder --example seek_probe -- <media>

use std::path::PathBuf;
use std::time::Instant;

use cutlass_core::RationalTime;
use cutlass_decoder::{OutputMode, open_video_decoder, probe};

fn secs(t: RationalTime) -> f64 {
    t.value as f64 * t.rate.den as f64 / t.rate.num as f64
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: seek_probe <media>");

    let info = probe(&path).expect("probe failed");
    println!(
        "{}x{} @ {:.3} fps, {} frames",
        info.width,
        info.height,
        info.frame_rate.as_f64(),
        info.frame_count
    );
    let fps = info.frame_rate;

    // 1. Fresh decoder, cold seek to various absolute times, one next_frame.
    for target_s in [5.0f64, 15.0, 30.0, 60.0, 90.0, 100.0] {
        let mut dec = open_video_decoder(&path, OutputMode::Cpu).expect("open");
        let tick = (target_s * fps.as_f64()).round() as i64;
        let target = RationalTime::new(tick, fps);
        let start = Instant::now();
        match dec.seek(target) {
            Ok(()) => match dec.next_frame() {
                Ok(Some(f)) => println!(
                    "cold seek {target_s:>6.1}s -> first frame pts {:>8.3}s  ({:>8.1} ms)",
                    secs(f.pts),
                    start.elapsed().as_secs_f64() * 1e3
                ),
                Ok(None) => println!("cold seek {target_s:>6.1}s -> EOS"),
                Err(e) => println!("cold seek {target_s:>6.1}s -> DECODE ERR: {e}"),
            },
            Err(e) => println!("cold seek {target_s:>6.1}s -> SEEK ERR: {e}"),
        }
    }

    // 2. Sequential decode across the bad GOP boundary at 20.854s: seek to
    //    19s (good sync at 10.43) and roll to 22s.
    {
        let mut dec = open_video_decoder(&path, OutputMode::Cpu).expect("open");
        let t19 = RationalTime::new((19.0 * fps.as_f64()).round() as i64, fps);
        dec.seek(t19).expect("seek 19s");
        let mut last = None;
        let mut err = None;
        loop {
            match dec.next_frame() {
                Ok(Some(f)) => {
                    let s = secs(f.pts);
                    last = Some(s);
                    if s >= 22.0 {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        match err {
            None => println!("sequential 19s -> 22s across boundary: OK (last pts {last:?})"),
            Some(e) => println!("sequential 19s -> 22s across boundary: ERR at {last:?}: {e}"),
        }
    }

    // 3. Manual fallback simulation (predates the decoder's built-in
    //    recovery; kept to compare costs): pull the seek start back until the
    //    reader starts at an earlier sync sample, walking forward to 60s.
    {
        let mut dec = open_video_decoder(&path, OutputMode::Cpu).expect("open");
        let target = RationalTime::new((60.0 * fps.as_f64()).round() as i64, fps);
        let start = Instant::now();
        match dec.frame_at(target) {
            Err(e) => println!("frame_at 60s: ERR: {e}"),
            Ok(Some(f)) => println!(
                "frame_at 60s: pts {:.3}s ({:.0} ms; recovery is built in)",
                secs(f.pts),
                start.elapsed().as_secs_f64() * 1e3
            ),
            Ok(None) => println!("frame_at 60s: None (past EOF for this asset)"),
        }
        // Back off in doubling steps until decode succeeds.
        let mut backoff_s = 1.0f64;
        loop {
            let earlier = 60.0 - backoff_s;
            if earlier < 0.0 {
                println!("fallback exhausted");
                break;
            }
            let t = RationalTime::new((earlier * fps.as_f64()).round() as i64, fps);
            dec.seek(t).expect("seek");
            let mut ok = None;
            loop {
                match dec.next_frame() {
                    Ok(Some(f)) => {
                        if secs(f.pts) >= 60.0 {
                            ok = Some(f.pts);
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            if let Some(pts) = ok {
                println!(
                    "fallback: start {:.1}s (backoff {backoff_s}s) -> pts {:.3}s, total {:.0} ms",
                    earlier,
                    secs(pts),
                    start.elapsed().as_secs_f64() * 1e3
                );
                break;
            }
            println!("fallback: start {:.1}s still fails", earlier);
            backoff_s *= 2.0;
        }
    }
}
