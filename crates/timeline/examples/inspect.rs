//! Load a project JSON and print structure / query active clips.
//!
//! ```text
//! cargo run -p timeline --example playground -- --export /tmp/demo.json
//! cargo run -p timeline --example inspect -- /tmp/demo.json
//! cargo run -p timeline --example inspect -- /tmp/demo.json --at 0 3 --at 0 22/1
//! ```

use std::env;
use std::fs;
use std::path::Path;
use std::process;

use decoder::Rational;
use timeline::deserialize_project;

fn usage() -> &'static str {
    "inspect <project.json> [--at <track_index> <time>]...

  Prints sources, tracks, clips, then optional active-clip queries.
  <time> is integer seconds or num/den (e.g. 22/1)."
}

fn parse_rational(word: &str) -> Rational {
    if let Some((n, d)) = word.split_once('/') {
        let num: i64 = n.parse().expect("numerator");
        let den: u32 = d.parse().expect("denominator");
        Rational::new(num, den).expect("rational")
    } else {
        Rational::new_raw(word.parse().expect("seconds"), 1)
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        println!("{}", usage());
        process::exit(if args.is_empty() { 1 } else { 0 });
    }

    let path = Path::new(&args[0]);
    let mut queries: Vec<(u64, Rational)> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--at" {
            let track: u64 = args
                .get(i + 1)
                .expect("--at needs track index")
                .parse()
                .expect("track index");
            let t = parse_rational(args.get(i + 2).expect("--at needs time"));
            queries.push((track, t));
            i += 3;
        } else {
            eprintln!("unknown argument: {}\n\n{}", args[i], usage());
            process::exit(1);
        }
    }

    let json = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("read {}: {e}", path.display());
        process::exit(1);
    });
    let project = deserialize_project(&json).unwrap_or_else(|e| {
        eprintln!("deserialize: {e}");
        process::exit(1);
    });

    println!("project {}", project.id.0);
    println!("schema_version {}", project.schema_version);
    println!(
        "settings {}x{} @ {}/{} fps",
        project.settings.width,
        project.settings.height,
        project.settings.frame_rate.num,
        project.settings.frame_rate.den
    );

    for src in project.sources.values() {
        println!(
            "source {} path={}",
            src.id,
            src.original_path.display()
        );
        if let Some(p) = &src.probed {
            let dur = p
                .duration
                .map(|d| d.to_string())
                .unwrap_or_else(|| "?".into());
            println!(
                "  probed {}x{} duration={dur} {:?}",
                p.width, p.height, p.pixel_format
            );
        }
    }

    for (ti, track) in project.tracks.iter().enumerate() {
        println!(
            "track[{ti}] id={} kind={:?} clips={}",
            track.id,
            track.kind,
            track.clips.len()
        );
        for clip in &track.clips {
            let end = clip
                .timeline_end()
                .map(|r| r.to_string())
                .unwrap_or_else(|| "?".into());
            println!(
                "  clip {} source={} timeline [{}, {}) source [{}, {})",
                clip.id,
                clip.source_id,
                clip.timeline_position,
                end,
                clip.source_in,
                clip.source_out
            );
        }
    }

    if queries.is_empty() {
        println!("\n(no --at queries; pass e.g. --at 0 3)");
        return;
    }

    for (track_index, t) in queries {
        let tid = project
            .tracks
            .get(track_index as usize)
            .map(|t| t.id)
            .unwrap_or_else(|| {
                eprintln!("track index {track_index} out of range");
                process::exit(1);
            });
        match project.active_clip_on_track(tid, t) {
            Ok(Some(a)) => println!(
                "at track[{track_index}] t={t}: clip {} source {} media_time {}",
                a.clip_id, a.source_id, a.media_time
            ),
            Ok(None) => println!("at track[{track_index}] t={t}: (gap)"),
            Err(e) => {
                eprintln!("at track[{track_index}] t={t}: {e}");
                process::exit(1);
            }
        }
    }
}
