//! Dump frames, e.g.:
//! `cargo run -p decoder --example dump_frames -- tests/assets/testsrc_h264.mp4 --seek 2 --exact --count 3`
//!
//! `--seek` accepts `num/den` (exact rational seconds), an integer string (exact `num/1`), or a decimal
//! (approximate, denom `1_000_000`).

use std::env;
use std::path::Path;

use decoder::{DecodeOutcome, Decoder, FrameData, Rational};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "Usage: dump_frames <path> [--seek <num/den|int|float>] [--exact|--scrub] [--count N]"
        );
        std::process::exit(1);
    }

    let path = Path::new(&args[0]);
    let mut seek: Option<Rational> = None;
    let mut exact = true;
    let mut count: usize = 10;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--seek" => {
                let s = args.get(i + 1).expect("--seek needs value");
                seek = Some(parse_rational(s));
                i += 2;
            }
            "--exact" => {
                exact = true;
                i += 1;
            }
            "--scrub" => {
                exact = false;
                i += 1;
            }
            "--count" => {
                count = args[i + 1].parse().expect("--count needs integer");
                i += 2;
            }
            other => panic!("unknown arg: {other}"),
        }
    }

    let mut dec = Decoder::open(path).expect("open");
    println!("info: {:?}", dec.info());

    if let Some(t) = seek {
        let out = if exact {
            dec.seek_exact(t)
        } else {
            dec.seek_scrub(t)
        }
        .expect("seek");
        match out {
            DecodeOutcome::Frame(f) => println!("after seek: pts {}", f.pts),
            DecodeOutcome::Eof => println!("after seek: EOF"),
        }
    }

    for n in 0..count {
        match dec.next_frame().expect("next") {
            DecodeOutcome::Frame(f) => {
                let FrameData::Cpu(cpu) = &f.data else {
                    unreachable!()
                };
                println!(
                    "frame {n}: pts {} planes={} plane0_len={}",
                    f.pts,
                    cpu.planes.len(),
                    cpu.planes[0].data.len()
                );
            }
            DecodeOutcome::Eof => {
                println!("frame {n}: EOF");
                break;
            }
        }
    }
}

fn parse_rational(s: &str) -> Rational {
    if let Some((a, b)) = s.split_once('/') {
        let num: i64 = a.parse().expect("seek num");
        let den: u32 = b.parse().expect("seek den");
        return Rational::new(num, den).expect("seek den non-zero");
    }
    if let Ok(n) = s.parse::<i64>() {
        return Rational::new_raw(n, 1);
    }
    let v: f64 = s.parse().expect("seek must be num/den, integer, or decimal");
    let scaled = (v * 1_000_000.0).round() as i64;
    Rational::new_raw(scaled, 1_000_000)
}
