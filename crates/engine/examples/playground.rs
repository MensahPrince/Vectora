//! CLI smoke test for the MVP engine API (`docs/engine/roadmap.md` phase 5).
//!
//! ```text
//! cargo run -p engine --example playground -- crates/engine/tests/assets/testsrc_h264.mp4
//! cargo run -p engine --example playground -- path/to/file.mp4 --script path/to/script.txt
//! ```

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use engine::{Engine, EngineEvent, Rational, SourceId};

fn parse_rational(word: &str) -> Rational {
    if let Some((n, d)) = word.split_once('/') {
        let num: i64 = n.trim().parse().expect("rational numerator");
        let den: u32 = d.trim().parse().expect("rational denominator");
        Rational::new(num, den).unwrap_or_else(|| panic!("invalid rational {word}"))
    } else {
        let sec: i64 = word.parse().expect("seconds or num/den");
        Rational::new_raw(sec, 1)
    }
}

#[derive(Debug)]
enum Step {
    Open,
    SeekExact(Rational),
    Scrub(Rational),
    Next(u32),
    Close,
    SleepMs(u64),
}

fn default_script() -> Vec<Step> {
    vec![
        Step::Open,
        Step::SeekExact(Rational::new_raw(0, 1)),
        Step::Next(5),
        Step::Scrub(Rational::new_raw(5, 2)),
        Step::SeekExact(Rational::new_raw(2, 1)),
        Step::Next(3),
        Step::Close,
    ]
}

fn load_script(path: &Path) -> Vec<Step> {
    let f = File::open(path).unwrap_or_else(|e| panic!("open script {}: {e}", path.display()));
    let mut steps = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line.expect("line");
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let cmd = it.next().expect("command");
        match cmd {
            "open" => steps.push(Step::Open),
            "seek_exact" => {
                let w = it.next().expect("seek_exact time");
                steps.push(Step::SeekExact(parse_rational(w)));
            }
            "scrub" => {
                let w = it.next().expect("scrub time");
                steps.push(Step::Scrub(parse_rational(w)));
            }
            "next" => {
                let n: u32 = it.next().expect("next count").parse().expect("next N");
                steps.push(Step::Next(n));
            }
            "close" => steps.push(Step::Close),
            "sleep_ms" => {
                let n: u64 = it.next().expect("sleep_ms").parse().expect("sleep_ms N");
                steps.push(Step::SleepMs(n));
            }
            other => panic!("unknown script command: {other}"),
        }
    }
    steps
}

fn main() {
    let mut args = std::env::args().skip(1);
    let media_path: PathBuf = args
        .next()
        .expect("usage: playground <media> [--script script.txt]")
        .into();
    let mut script_path: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        if a == "--script" {
            script_path = Some(
                args.next()
                    .expect("--script needs a path")
                    .into(),
            );
        } else {
            panic!("unexpected arg {a}");
        }
    }

    let steps = match &script_path {
        Some(p) => load_script(p),
        None => default_script(),
    };

    let (engine, rx) = Engine::new();

    let printer = thread::spawn(move || {
        while let Ok(ev) = rx.recv() {
            match &ev {
                EngineEvent::Opened {
                    source_id,
                    info,
                    request_id,
                } => {
                    println!(
                        "[opened] source={source_id} request={request_id} {}x{} {:?}",
                        info.width, info.height, info.pixel_format
                    );
                }
                EngineEvent::Frame {
                    source_id,
                    frame,
                    request_id,
                } => {
                    println!(
                        "[frame] source={source_id} request={request_id:?} pts={} {}x{}",
                        frame.pts,
                        frame.width,
                        frame.height
                    );
                }
                EngineEvent::Eof {
                    source_id,
                    request_id,
                } => {
                    println!("[eof] source={source_id} request={request_id:?}");
                }
                EngineEvent::Error {
                    source_id,
                    error,
                    request_id,
                } => {
                    println!(
                        "[error] source={source_id:?} request={request_id:?} {error}"
                    );
                }
                EngineEvent::Closed { source_id } => {
                    println!("[closed] source={source_id}");
                }
            }
        }
        println!("[consumer] event channel disconnected");
    });

    let mut current_source: Option<SourceId> = None;

    for step in steps {
        match step {
            Step::Open => {
                let (sid, _rid) = engine.open(media_path.clone());
                current_source = Some(sid);
                thread::sleep(Duration::from_millis(20));
            }
            Step::SeekExact(t) => {
                let sid = current_source.expect("open before seek_exact");
                let _ = engine.seek_exact(sid, t);
                thread::sleep(Duration::from_millis(15));
            }
            Step::Scrub(t) => {
                let sid = current_source.expect("open before scrub");
                engine.seek_scrub(sid, t);
                thread::sleep(Duration::from_millis(15));
            }
            Step::Next(n) => {
                let sid = current_source.expect("open before next");
                for _ in 0..n {
                    engine.next_frame(sid);
                    thread::sleep(Duration::from_millis(10));
                }
            }
            Step::Close => {
                if let Some(sid) = current_source.take() {
                    engine.close(sid);
                    thread::sleep(Duration::from_millis(15));
                }
            }
            Step::SleepMs(ms) => thread::sleep(Duration::from_millis(ms)),
        }
    }

    drop(engine);
    printer.join().expect("printer join");
}
