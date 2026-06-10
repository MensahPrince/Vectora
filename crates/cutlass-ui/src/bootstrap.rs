//! Build a demo session inside the engine (import assets when available).

use std::path::{Path, PathBuf};

use cutlass_commands::{Command, EditCommand, EditOutcome, ProjectCommand};
use cutlass_engine::{ApplyOutcome, Engine, EngineError};
use cutlass_models::{Generator, Rational, RationalTime, TimeRange, TrackKind};

pub fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

pub fn small_video_asset() -> Option<PathBuf> {
    let dir = assets_dir();
    for name in [
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

fn rt(value: i64) -> RationalTime {
    RationalTime::new(value, Rational::FPS_24)
}

fn tr(start: i64, duration: i64) -> TimeRange {
    TimeRange::at_rate(start, duration, Rational::FPS_24)
}

fn import(engine: &mut Engine, path: &Path) -> Result<cutlass_models::MediaId, EngineError> {
    match engine.apply(Command::Project(ProjectCommand::Import {
        path: path.to_path_buf(),
    }))? {
        ApplyOutcome::Imported { media } => Ok(media),
        other => Err(EngineError::Preview(format!("unexpected import outcome: {other:?}"))),
    }
}

fn add_track(engine: &mut Engine, kind: TrackKind, name: &str) -> Result<cutlass_models::TrackId, EngineError> {
    match engine.apply(Command::Edit(EditCommand::AddTrack {
        kind,
        name: name.into(),
    }))? {
        ApplyOutcome::Edited(EditOutcome::CreatedTrack(id)) => Ok(id),
        other => Err(EngineError::Preview(format!("unexpected add-track outcome: {other:?}"))),
    }
}

fn add_media_clip(
    engine: &mut Engine,
    track: cutlass_models::TrackId,
    media: cutlass_models::MediaId,
    source: TimeRange,
    start: RationalTime,
) -> Result<(), EngineError> {
    match engine.apply(Command::Edit(EditCommand::AddClip {
        track,
        media,
        source,
        start,
    }))? {
        ApplyOutcome::Edited(EditOutcome::Created(_)) => Ok(()),
        other => Err(EngineError::Preview(format!("unexpected add-clip outcome: {other:?}"))),
    }
}

fn add_solid(
    engine: &mut Engine,
    track: cutlass_models::TrackId,
    timeline: TimeRange,
    rgba: [u8; 4],
) -> Result<(), EngineError> {
    match engine.apply(Command::Edit(EditCommand::AddGenerated {
        track,
        generator: Generator::SolidColor { rgba },
        timeline,
    }))? {
        ApplyOutcome::Edited(EditOutcome::Created(_)) => Ok(()),
        other => Err(EngineError::Preview(format!("unexpected add-generated outcome: {other:?}"))),
    }
}

/// Populate `engine` with a small editable timeline for UI development.
pub fn bootstrap_demo(engine: &mut Engine) -> Result<(), EngineError> {
    if let Some(path) = small_video_asset() {
        let media = import(engine, &path)?;
        let duration = engine
            .project()
            .media(media)
            .map(|m| m.duration.value.min(240))
            .unwrap_or(96);

        let v1 = add_track(engine, TrackKind::Video, "V1")?;
        add_media_clip(engine, v1, media, tr(0, duration), rt(0))?;

        let v2 = add_track(engine, TrackKind::Video, "V2")?;
        let second_len = (duration / 2).max(24);
        add_media_clip(
            engine,
            v2,
            media,
            tr(second_len, second_len.min(duration - second_len)),
            rt(duration + 24),
        )?;

        let a1 = add_track(engine, TrackKind::Audio, "A1")?;
        add_solid(engine, a1, tr(0, duration), [0xC9, 0x98, 0x46, 0xFF])?;
    } else {
        let v1 = add_track(engine, TrackKind::Video, "V1")?;
        add_solid(engine, v1, tr(0, 96), [0x4A, 0x6F, 0xA5, 0xFF])?;
        let v2 = add_track(engine, TrackKind::Video, "V2")?;
        add_solid(engine, v2, tr(120, 60), [0x5E, 0x8B, 0x7E, 0xFF])?;
    }
    Ok(())
}
