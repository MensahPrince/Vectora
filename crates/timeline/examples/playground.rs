//! Build a demo project, run edit commands, query active clips, undo/redo.
//!
//! ```text
//! cargo run -p timeline --example playground
//! cargo run -p timeline --example playground -- --script examples/demo.script
//! cargo run -p timeline --example playground -- --import /tmp/project.json --export /tmp/out.json
//! ```

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process;

use decoder::{PixelFormat, Rational, SourceInfo};
use timeline::{
    deserialize_project, serialize_project, AddClip, AddSource, AddTrack, Clip, ClipId,
    MediaSourceId, MoveClip, Project, RemoveClip, SetSourceProbed, TrackId, TrackKind,
    TrimClipIn, TrimClipOut,
};

fn usage() -> &'static str {
    "playground [--script script.txt] [--import project.json] [--export project.json]

  Default: run a built-in demo (add source + clip, query times, move, trim, undo).

  --script   Run commands from a script file (see examples/demo.script).
  --import   Start from a serialized project JSON.
  --export   Write project JSON after all steps."
}

fn parse_rational(word: &str) -> Rational {
    if let Some((n, d)) = word.split_once('/') {
        let num: i64 = n.trim().parse().unwrap_or_else(|_| panic!("bad num in {word:?}"));
        let den: u32 = d.trim().parse().unwrap_or_else(|_| panic!("bad den in {word:?}"));
        Rational::new(num, den).unwrap_or_else(|| panic!("invalid rational {word}"))
    } else {
        let sec: i64 = word.parse().unwrap_or_else(|_| panic!("expected seconds or n/d: {word}"));
        Rational::new_raw(sec, 1)
    }
}

#[derive(Debug)]
enum Step {
    AddSource(PathBuf),
    AddTrack(TrackKind),
    Probe { source: u64, duration_secs: i64 },
    AddClip {
        track: u64,
        timeline_pos: Rational,
        source_in: Rational,
        source_out: Rational,
    },
    At { track: u64, time: Rational },
    Move { clip: u64, pos: Rational },
    TrimIn { clip: u64, new_in: Rational },
    TrimOut { clip: u64, new_out: Rational },
    RemoveClip(u64),
    Undo,
    Redo,
    Print,
}

fn default_steps() -> Vec<Step> {
    vec![
        Step::AddSource(PathBuf::from("/media/demo.mp4")),
        Step::Probe {
            source: 0,
            duration_secs: 120,
        },
        Step::AddClip {
            track: 0,
            timeline_pos: Rational::new_raw(0, 1),
            source_in: Rational::new_raw(0, 1),
            source_out: Rational::new_raw(10, 1),
        },
        Step::Print,
        Step::At {
            track: 0,
            time: Rational::new_raw(3, 1),
        },
        Step::At {
            track: 0,
            time: Rational::new_raw(9, 1),
        },
        Step::Move {
            clip: 0,
            pos: Rational::new_raw(20, 1),
        },
        Step::At {
            track: 0,
            time: Rational::new_raw(22, 1),
        },
        Step::TrimIn {
            clip: 0,
            new_in: Rational::new_raw(2, 1),
        },
        Step::At {
            track: 0,
            time: Rational::new_raw(21, 1),
        },
        Step::Undo,
        Step::At {
            track: 0,
            time: Rational::new_raw(22, 1),
        },
        Step::Redo,
    ]
}

fn load_script(path: &Path) -> Vec<Step> {
    let f = File::open(path).unwrap_or_else(|e| panic!("open script {}: {e}", path.display()));
    let mut steps = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line.expect("read line").trim().to_string();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let cmd = it.next().expect("command");
        match cmd {
            "add_source" => {
                let p = it.next().expect("add_source path");
                steps.push(Step::AddSource(p.into()));
            }
            "add_track" => {
                let kind = match it.next().unwrap_or("video") {
                    "video" => TrackKind::Video,
                    other => panic!("unknown track kind: {other}"),
                };
                steps.push(Step::AddTrack(kind));
            }
            "probe" => {
                let sid: u64 = it.next().expect("probe source_id").parse().expect("source_id");
                let dur: i64 = it.next().expect("probe duration").parse().expect("duration");
                steps.push(Step::Probe {
                    source: sid,
                    duration_secs: dur,
                });
            }
            "add_clip" => {
                let track: u64 = it.next().expect("track").parse().expect("track");
                let pos = parse_rational(it.next().expect("timeline_pos"));
                let sin = parse_rational(it.next().expect("source_in"));
                let sout = parse_rational(it.next().expect("source_out"));
                steps.push(Step::AddClip {
                    track,
                    timeline_pos: pos,
                    source_in: sin,
                    source_out: sout,
                });
            }
            "at" => {
                let track: u64 = it.next().expect("track").parse().expect("track");
                let t = parse_rational(it.next().expect("time"));
                steps.push(Step::At { track, time: t });
            }
            "move" => {
                let clip: u64 = it.next().expect("clip").parse().expect("clip");
                let pos = parse_rational(it.next().expect("pos"));
                steps.push(Step::Move { clip, pos });
            }
            "trim_in" => {
                let clip: u64 = it.next().expect("clip").parse().expect("clip");
                let t = parse_rational(it.next().expect("new_in"));
                steps.push(Step::TrimIn { clip, new_in: t });
            }
            "trim_out" => {
                let clip: u64 = it.next().expect("clip").parse().expect("clip");
                let t = parse_rational(it.next().expect("new_out"));
                steps.push(Step::TrimOut { clip, new_out: t });
            }
            "remove_clip" => {
                let clip: u64 = it.next().expect("clip").parse().expect("clip");
                steps.push(Step::RemoveClip(clip));
            }
            "undo" => steps.push(Step::Undo),
            "redo" => steps.push(Step::Redo),
            "print" => steps.push(Step::Print),
            other => panic!("unknown script command: {other}"),
        }
    }
    steps
}

fn track_id(project: &Project, index: u64) -> TrackId {
    project
        .tracks
        .get(index as usize)
        .map(|t| t.id)
        .unwrap_or_else(|| panic!("track index {index} out of range ({} tracks)", project.tracks.len()))
}

fn print_project(project: &Project) {
    println!("--- project {} (schema {}) ---", project.id.0, project.schema_version);
    println!("  sources: {}", project.sources.len());
    for src in project.sources.values() {
        let dur = src
            .probed
            .as_ref()
            .and_then(|p| p.duration)
            .map(|d| d.to_string())
            .unwrap_or_else(|| "?".into());
        println!(
            "    {} -> {} (probed duration: {dur})",
            src.id,
            src.original_path.display()
        );
    }
    for track in &project.tracks {
        println!(
            "  track {} {:?} clips={} muted={} locked={}",
            track.id,
            track.kind,
            track.clips.len(),
            track.muted,
            track.locked
        );
        for clip in &track.clips {
            let end = clip
                .timeline_end()
                .map(|r| r.to_string())
                .unwrap_or_else(|| "?".into());
            println!(
                "    clip {} source={} timeline=[{}, {}) source=[{}, {})",
                clip.id,
                clip.source_id,
                clip.timeline_position,
                end,
                clip.source_in,
                clip.source_out,
            );
        }
    }
    println!(
        "  history: undo={} redo={}",
        project.history.undo_depth(),
        project.history.redo_depth()
    );
}

fn print_at(project: &Project, track_index: u64, t: Rational) {
    let tid = track_id(project, track_index);
    match project.active_clip_on_track(tid, t) {
        Ok(Some(active)) => {
            println!(
                "[at] track {track_index} t={t} -> clip {} source {} media_time {}",
                active.clip_id, active.source_id, active.media_time
            );
        }
        Ok(None) => println!("[at] track {track_index} t={t} -> (no clip)"),
        Err(e) => println!("[at] track {track_index} t={t} -> error: {e}"),
    }
}

fn run_steps(project: &mut Project, steps: &[Step]) {
    for step in steps {
        match step {
            Step::AddSource(path) => {
                project
                    .apply(Box::new(AddSource::new(path.clone())), true)
                    .unwrap_or_else(|e| panic!("add_source: {e}"));
                let sid = *project.sources.keys().next().unwrap();
                println!("[cmd] add_source {} -> {sid}", path.display());
            }
            Step::AddTrack(kind) => {
                project
                    .apply(Box::new(AddTrack::new(*kind)), true)
                    .unwrap_or_else(|e| panic!("add_track: {e}"));
                let tid = project.tracks.last().unwrap().id;
                println!("[cmd] add_track {kind:?} -> {tid}");
            }
            Step::Probe { source, duration_secs } => {
                let sid = MediaSourceId(*source);
                let info = SourceInfo {
                    width: 1920,
                    height: 1080,
                    timebase: Rational::new_raw(1, 30_000),
                    duration: Some(Rational::new_raw(*duration_secs, 1)),
                    pixel_format: PixelFormat::Yuv420p,
                };
                project
                    .apply(Box::new(SetSourceProbed::new(sid, info)), false)
                    .unwrap_or_else(|e| panic!("probe: {e}"));
                println!("[cmd] probe source {source} duration {duration_secs}s");
            }
            Step::AddClip {
                track,
                timeline_pos,
                source_in,
                source_out,
            } => {
                let tid = track_id(project, *track);
                let source_id = project
                    .sources
                    .keys()
                    .next()
                    .copied()
                    .expect("add_source before add_clip");
                let clip_id = project.alloc_clip_id();
                let clip = Clip {
                    id: clip_id,
                    source_id,
                    source_in: *source_in,
                    source_out: *source_out,
                    timeline_position: *timeline_pos,
                };
                project
                    .apply(Box::new(AddClip::new(tid, clip)), true)
                    .unwrap_or_else(|e| panic!("add_clip: {e}"));
                println!("[cmd] add_clip track {track} -> {clip_id} at {timeline_pos}");
            }
            Step::At { track, time } => print_at(project, *track, *time),
            Step::Move { clip, pos } => {
                project
                    .apply(Box::new(MoveClip::new(ClipId(*clip), *pos)), true)
                    .unwrap_or_else(|e| panic!("move: {e}"));
                println!("[cmd] move clip {clip} -> {pos}");
            }
            Step::TrimIn { clip, new_in } => {
                project
                    .apply(Box::new(TrimClipIn::new(ClipId(*clip), *new_in)), true)
                    .unwrap_or_else(|e| panic!("trim_in: {e}"));
                println!("[cmd] trim_in clip {clip} -> {new_in}");
            }
            Step::TrimOut { clip, new_out } => {
                project
                    .apply(Box::new(TrimClipOut::new(ClipId(*clip), *new_out)), true)
                    .unwrap_or_else(|e| panic!("trim_out: {e}"));
                println!("[cmd] trim_out clip {clip} -> {new_out}");
            }
            Step::RemoveClip(clip) => {
                project
                    .apply(Box::new(RemoveClip::new(ClipId(*clip))), true)
                    .unwrap_or_else(|e| panic!("remove_clip: {e}"));
                println!("[cmd] remove_clip {clip}");
            }
            Step::Undo => {
                let ok = project.undo().unwrap();
                println!("[cmd] undo -> {ok}");
            }
            Step::Redo => {
                let ok = project.redo().unwrap();
                println!("[cmd] redo -> {ok}");
            }
            Step::Print => print_project(project),
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut script_path: Option<PathBuf> = None;
    let mut import_path: Option<PathBuf> = None;
    let mut export_path: Option<PathBuf> = None;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                return;
            }
            "--script" => {
                script_path = Some(
                    args.next()
                        .unwrap_or_else(|| {
                            eprintln!("error: --script requires a path\n\n{}", usage());
                            process::exit(1);
                        })
                        .into(),
                );
            }
            "--import" => {
                import_path = Some(
                    args.next()
                        .unwrap_or_else(|| {
                            eprintln!("error: --import requires a path\n\n{}", usage());
                            process::exit(1);
                        })
                        .into(),
                );
            }
            "--export" => {
                export_path = Some(
                    args.next()
                        .unwrap_or_else(|| {
                            eprintln!("error: --export requires a path\n\n{}", usage());
                            process::exit(1);
                        })
                        .into(),
                );
            }
            other => {
                eprintln!("error: unknown argument {other:?}\n\n{}", usage());
                process::exit(1);
            }
        }
    }

    let mut project = if let Some(path) = import_path {
        let json = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        deserialize_project(&json).unwrap_or_else(|e| panic!("deserialize: {e}"))
    } else {
        Project::new().with_default_video_track()
    };

    let steps: Vec<Step> = match script_path {
        Some(p) => load_script(&p),
        None if project.sources.is_empty() && project.tracks.iter().all(|t| t.clips.is_empty()) => {
            default_steps()
        }
        None => {
            eprintln!("playground: loaded project; no script (use --script or --export only)");
            Vec::new()
        }
    };

    if !steps.is_empty() {
        run_steps(&mut project, &steps);
    } else {
        print_project(&project);
    }

    if let Some(path) = export_path {
        let json = serialize_project(&project).expect("serialize");
        fs::write(&path, &json).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        println!("[export] wrote {} ({} bytes)", path.display(), json.len());
    }
}
