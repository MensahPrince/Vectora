//! End-to-end session smoke test: new project → import (up to 3 media) →
//! multi-clip edit → save → export.
//!
//! Writes under `.cutlass/` in the current working directory:
//!   - `projects/<name>.cutlass` — saved project
//!   - `exports/<name>.mp4` — rendered timeline you can open in any player
//!   - `cache/` — frame cache (preview path; export decodes originals)

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use cutlass_commands::{Command, EditCommand, EditOutcome, ProjectCommand};
use cutlass_engine::{ApplyOutcome, Engine, EngineConfig};
use cutlass_models::{MediaId, RationalTime, TimeRange, TrackId, TrackKind, resample};

use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

type AnyError = Box<dyn Error + Send + Sync>;

const ASSETS_DIR: &str = "assets";
const CUTLASS_DIR: &str = ".cutlass";
const PROJECTS_SUBDIR: &str = "projects";
const EXPORTS_SUBDIR: &str = "exports";
const CACHE_SUBDIR: &str = "cache";
const MAX_MEDIA: usize = 3;
const MAX_CLIP_FRAMES: i64 = 96;

struct Args {
    name: String,
}

fn setup_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn parse_args() -> Result<Args, AnyError> {
    let mut name = env::var("CUTLASS_NAME").unwrap_or_else(|_| "demo".into());
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--name" | "-n" => {
                name = iter.next().ok_or("missing value after --name")?;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag: {other}").into());
            }
            path => {
                return Err(format!(
                    "unexpected argument: {path} (videos are picked randomly from {ASSETS_DIR}/)"
                )
                .into());
            }
        }
    }

    if name.is_empty() {
        return Err("session name must not be empty".into());
    }

    Ok(Args { name })
}

fn print_usage() {
    eprintln!(
        "Usage: cutlass-app [--name NAME]\n\
         \n\
         Builds a three-clip project from random videos in assets/, saves to\n\
         .cutlass/projects/, exports MP4 to .cutlass/exports/.\n\
         \n\
         --name  Session basename (default: demo, or CUTLASS_NAME env)\n"
    );
}

fn video_assets_in(dir: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "mp4"))
        .collect();
    paths.sort();
    paths
}

fn random_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (u64::from(std::process::id()) << 32)
}

fn next_random(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
    *state
}

/// [`MAX_MEDIA`] random picks from `assets/`; distinct when enough files exist.
fn random_session_videos() -> Result<Vec<PathBuf>, AnyError> {
    let pool = video_assets_in(ASSETS_DIR);
    if pool.is_empty() {
        return Err(format!("no mp4 found in {ASSETS_DIR}/").into());
    }

    let mut rng = random_seed();
    let mut available: Vec<usize> = (0..pool.len()).collect();
    let mut picks = Vec::with_capacity(MAX_MEDIA);

    for _ in 0..MAX_MEDIA {
        if available.is_empty() {
            available = (0..pool.len()).collect();
        }
        let slot = (next_random(&mut rng) as usize) % available.len();
        let pool_idx = available.swap_remove(slot);
        picks.push(pool[pool_idx].clone());
    }

    Ok(picks)
}

fn import_media(engine: &mut Engine, path: &Path) -> Result<MediaId, AnyError> {
    match engine.apply(Command::Project(ProjectCommand::Import {
        path: path.to_path_buf(),
    }))? {
        ApplyOutcome::Imported { media } => Ok(media),
        other => Err(format!("import failed for {}: {other:?}", path.display()).into()),
    }
}

fn add_media_clip(
    engine: &mut Engine,
    track: TrackId,
    media: MediaId,
    source: TimeRange,
    start: RationalTime,
) -> Result<(), AnyError> {
    match engine.apply(Command::Edit(EditCommand::AddClip {
        track,
        media,
        source,
        start,
    }))? {
        ApplyOutcome::Edited(EditOutcome::Created(_)) => Ok(()),
        other => Err(format!("add clip failed: {other:?}").into()),
    }
}

fn cutlass_paths(name: &str) -> Result<(PathBuf, PathBuf, PathBuf), AnyError> {
    let root = PathBuf::from(CUTLASS_DIR);
    fs::create_dir_all(root.join(PROJECTS_SUBDIR))?;
    fs::create_dir_all(root.join(EXPORTS_SUBDIR))?;
    fs::create_dir_all(root.join(CACHE_SUBDIR))?;
    let project = root.join(PROJECTS_SUBDIR).join(format!("{name}.cutlass"));
    let export = root.join(EXPORTS_SUBDIR).join(format!("{name}.mp4"));
    let cache = root.join(CACHE_SUBDIR);
    Ok((project, export, cache))
}

fn abs(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn run() -> Result<(), AnyError> {
    cutlass_engine::init();
    let args = parse_args()?;
    let (project_path, export_path, cache_dir) = cutlass_paths(&args.name)?;

    let videos = random_session_videos()?;
    info!(
        name = %args.name,
        picks = videos.len(),
        "starting e2e session"
    );
    for (i, path) in videos.iter().enumerate() {
        info!(clip = i + 1, video = %path.file_name().and_then(|n| n.to_str()).unwrap_or("?"), "selected asset");
    }

    let config = EngineConfig {
        cache_dir,
        ..EngineConfig::default()
    };

    let mut engine = Engine::new(config)?;

    let mut media_ids = Vec::with_capacity(MAX_MEDIA);
    for path in &videos {
        media_ids.push(import_media(&mut engine, path)?);
    }
    info!(
        media = media_ids.len(),
        clips = MAX_MEDIA,
        "imported media for multi-clip session"
    );

    let track_id = match engine.apply(Command::Edit(EditCommand::AddTrack {
        kind: TrackKind::Video,
        name: "V1".into(),
        index: None,
    }))? {
        ApplyOutcome::Edited(EditOutcome::CreatedTrack(id)) => id,
        other => return Err(format!("add track failed: {other:?}").into()),
    };

    let tl_rate = engine.project().timeline().frame_rate;
    let mut timeline_start = RationalTime::new(0, tl_rate);

    for media_id in media_ids {
        let (clip_len, media_rate) = {
            let media = engine
                .project()
                .media(media_id)
                .ok_or("imported media missing from pool")?;
            (
                media.duration.value.clamp(1, MAX_CLIP_FRAMES),
                media.frame_rate,
            )
        };
        let source = TimeRange::at_rate(0, clip_len, media_rate);
        let timeline_duration = resample(RationalTime::new(clip_len, media_rate), tl_rate)
            .value
            .max(1);
        add_media_clip(&mut engine, track_id, media_id, source, timeline_start)?;
        timeline_start = RationalTime::new(timeline_start.value + timeline_duration, tl_rate);
    }

    info!(
        clips = engine.project().timeline().clip_count(),
        frames = engine.project().timeline().duration().value,
        "timeline assembled"
    );

    let preview = engine
        .get_frame(RationalTime::new(0, tl_rate))
        .map_err(|e| format!("preview failed: {e}"))?;
    info!(
        width = preview.width,
        height = preview.height,
        bytes = preview.bytes.len(),
        "preview frame ok"
    );

    match engine.apply(Command::Project(ProjectCommand::Save {
        path: project_path.clone(),
    }))? {
        ApplyOutcome::Saved => {}
        other => return Err(format!("save failed: {other:?}").into()),
    };

    let export_start = Instant::now();
    let stats = match engine.apply(Command::Project(ProjectCommand::Export {
        path: export_path.clone(),
    }))? {
        ApplyOutcome::Exported { stats } => stats,
        other => return Err(format!("export failed: {other:?}").into()),
    };

    info!(
        frames = stats.frames,
        width = stats.width,
        height = stats.height,
        elapsed = ?export_start.elapsed(),
        project = %abs(&project_path).display(),
        export = %abs(&export_path).display(),
        "e2e session complete — open the MP4 path above in QuickTime/VLC"
    );

    Ok(())
}

fn main() {
    setup_tracing();
    if let Err(e) = run() {
        warn!(error = %e, "cutlass-app failed");
        std::process::exit(1);
    }
}
