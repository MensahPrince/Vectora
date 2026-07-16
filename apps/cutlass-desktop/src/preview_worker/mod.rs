//! Background preview rendering: engine and decode/composite stay off the UI thread.
//!
//! Ported from main's crates/cutlass-ui onto this branch's engine: engine
//! ownership, the full edit/project message set, debounced autosave, the
//! fit-sized preview pump, audio snapshots, thumbnail/strip registration,
//! export, live gesture/generator overrides, and the AI agent bridge.

mod dispatch;
mod frame_cache;
mod frame_fit;
mod handle;
mod render;
mod rpc;
#[cfg(test)]
mod tests;
mod types;
mod worker_loop;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, bounded, unbounded};
use cutlass_commands::{Command, EditCommand, EditOutcome, ProjectCommand, TemplatePick};
use cutlass_engine::{ApplyOutcome, Engine, EngineConfig, SeekPolicy};
use cutlass_models::{
    AnimatedTransform, ClipId, ClipParam, ClipSource, ClipTransform, ColorAdjustments, CropRect,
    Easing, Filter, Generator, LinkId, Lut, MAX_SPEED, MIN_SPEED, MarkerColor, MarkerId, MediaId,
    Param, ParamValue, Project, Rational, RationalTime, TimeRange, Track, TrackId, TrackKind,
    resample,
};
use cutlass_render::{ExportSettings, RenderError, Renderer};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use tracing::{debug, error, info, warn};

use crate::agent::{AgentCreated, AgentPlanStep};
use crate::proxy::ProxyHandle;
use crate::strips::StripHandle;
use crate::thumbnails::{ThumbKind, ThumbnailHandle};
use crate::{EditorStore, ExportBackend, PreviewStore};

use dispatch::*;
use frame_cache::*;
use frame_fit::*;
use render::*;
use rpc::*;
use types::*;
// `PreviewWorker` (the other `pub` item in `worker_loop`) is re-exported
// explicitly below; this one is a testable seam exercised directly by
// `preview_worker::tests` and otherwise unused outside `worker_loop` itself.
#[allow(unused_imports)]
use worker_loop::message_invalidates_preview;

pub(crate) use rpc::ProjectMaintenanceGuard;
pub(crate) use types::{
    ApplyTemplateRpcResult, ImportMediaRpcResult, NewProjectRpcResult, OpenProjectRpcResult,
    PreviewCacheStats, RelinkFolderRpcResult, RelinkMediaRpcResult, SaveProjectRpcResult,
};
pub use types::{ExportRequest, GroupMove, PreviewSession, TrackFlag, WorkerHandle};
pub use worker_loop::PreviewWorker;

/// Fit/fill helper (M1 canvas settings): compute the centered fit (scale
/// 1.0) or cover transform for a clip and commit it through the regular
/// `SetClipTransform` path, so it keyframes at the playhead on animated
/// clips and undoes in one step like any gesture.
fn fit_clip_and_publish(
    engine: &mut Engine,
    clip: &str,
    fill: bool,
    tick: i64,
    tl_rate: Rational,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "fit/fill ignored: unparsable clip id");
        return;
    };
    let Some(transform) = fit_clip_transform(engine, clip_id, fill, tick) else {
        error!(%clip_id, "fit/fill ignored: unknown clip or degenerate content");
        return;
    };
    let at = RationalTime::new(tick, tl_rate);
    set_transform_and_publish(engine, clip, transform, at, ui);
}

/// The transform that centers a clip at aspect-fit (scale 1.0 by the
/// placement convention) or at the cover scale that fills the canvas — the
/// crop's kept region is what aspect-fits, so it is also what must cover.
/// Rotation and opacity keep their playhead-sampled values; position resets
/// to center (CapCut fit/fill semantics).
fn fit_clip_transform(
    engine: &Engine,
    clip_id: ClipId,
    fill: bool,
    tick: i64,
) -> Option<ClipTransform> {
    let project = engine.project();
    let clip = project.clip(clip_id)?;
    let (canvas_w, canvas_h) = cutlass_render::canvas_size(project);
    let (content_w, content_h) = match clip.media() {
        Some(media_id) => {
            let media = project.media(media_id)?;
            (media.width, media.height)
        }
        // Generators raster at canvas size: fit and fill are both 1.0.
        None => (canvas_w, canvas_h),
    };
    let (w, h) = (
        content_w as f32 * clip.crop.w,
        content_h as f32 * clip.crop.h,
    );
    if w <= 0.0 || h <= 0.0 || canvas_w == 0 || canvas_h == 0 {
        return None;
    }
    let (cw, ch) = (canvas_w as f32, canvas_h as f32);
    let fit = (cw / w).min(ch / h);
    let cover = (cw / w).max(ch / h);
    let scale = if fill { cover / fit } else { 1.0 };
    let sampled = clip.transform.sample_at(clip.animation_tick_f(tick as f64));
    Some(ClipTransform {
        position: [0.0, 0.0],
        anchor_point: sampled.anchor_point,
        scale,
        rotation: sampled.rotation,
        opacity: sampled.opacity,
    })
}

/// Commit a transform gesture as one undoable `SetClipTransform`, keyframing
/// at `at` (the playhead) when the property is animated.
fn set_transform_and_publish(
    engine: &mut Engine,
    clip: &str,
    transform: ClipTransform,
    at: RationalTime,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-transform ignored: unparsable clip id");
        return;
    };
    // CapCut compose semantics: on a clip with animated properties this
    // commit writes keyframes at the playhead instead of flattening. Note it
    // before applying so the UI can surface "a gesture added a keyframe".
    let wrote_keyframe = engine
        .project()
        .clip(clip_id)
        .is_some_and(|c| c.transform.is_animated());
    match engine.apply(Command::Edit(EditCommand::SetClipTransform {
        clip: clip_id,
        transform,
        at: Some(at),
    })) {
        Ok(_) => {
            info!(%clip_id, ?transform, "set clip transform");
            if wrote_keyframe {
                bump_keyframe_commit_epoch(ui);
            }
            publish_projection(engine, ui);
        }
        Err(e) => {
            error!(%clip_id, "set transform failed: {e}");
            publish_projection(engine, ui);
        }
    }
}

/// Point the engine's transform override at `clip` (raw id) for the next
/// renders — the live preview of an in-flight gesture. Unparsable ids are
/// dropped (stale projection race).
fn apply_transform_override(engine: &mut Engine, clip: &str, transform: ClipTransform) {
    match parse_raw_id(clip).map(ClipId::from_raw) {
        Some(id) => engine.set_transform_override(Some((id, transform))),
        None => error!(clip, "transform override ignored: unparsable clip id"),
    }
}

/// Point the engine's generator override at `clip` (raw id) for the next
/// renders — the live preview of an uncommitted inspector edit. Unparsable
/// ids are dropped (stale projection race), same as the transform override.
fn apply_generator_override(engine: &mut Engine, clip: &str, generator: Generator) {
    match parse_raw_id(clip).map(ClipId::from_raw) {
        Some(id) => engine.set_generator_override(Some((id, generator))),
        None => error!(clip, "generator override ignored: unparsable clip id"),
    }
}

/// Point the engine's look override at `clip` (raw id) for the next renders —
/// the live preview of an uncommitted filter/adjustment edit. Unparsable ids
/// are dropped (stale projection race), same as the other overrides.
fn apply_look_override(
    engine: &mut Engine,
    clip: &str,
    filter_id: &str,
    intensity: f32,
    adjust: ColorAdjustments,
) {
    match parse_raw_id(clip).map(ClipId::from_raw) {
        Some(id) => engine.set_look_override(Some((
            id,
            filter_from_ui(filter_id, intensity),
            sanitize_adjustments(adjust),
        ))),
        None => error!(clip, "look override ignored: unparsable clip id"),
    }
}

fn filter_from_ui(filter_id: &str, intensity: f32) -> Option<Filter> {
    let id = filter_id.trim();
    if id.is_empty() {
        return None;
    }
    Some(Filter {
        id: id.to_string(),
        intensity: clamp_unit(intensity),
    })
}

fn sanitize_adjustments(adjust: ColorAdjustments) -> ColorAdjustments {
    ColorAdjustments {
        brightness: clamp_signed_unit(adjust.brightness),
        contrast: clamp_signed_unit(adjust.contrast),
        saturation: clamp_signed_unit(adjust.saturation),
        exposure: clamp_signed_unit(adjust.exposure),
        temperature: clamp_signed_unit(adjust.temperature),
    }
}

fn clamp_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn clamp_signed_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Signal the inspector that a transform gesture just wrote keyframes (the
/// transient "keyframe added" chip): bump `EditorStore.keyframe-commit-epoch`.
fn bump_keyframe_commit_epoch(ui: &UiSink) {
    let editor_weak = ui.editor.clone();
    if let Err(e) = slint::invoke_from_event_loop(move || {
        if let Some(store) = editor_weak.upgrade() {
            store.set_keyframe_commit_epoch(store.get_keyframe_commit_epoch().wrapping_add(1));
        }
    }) {
        error!("failed to bump keyframe commit epoch: {e}");
    }
}

/// Insert or replace one property keyframe at `at` (absolute playhead
/// position) as one undoable edit (keyframes roadmap Phase 1: the inspector
/// diamond / easing picker). Engine-rejected positions (playhead outside the
/// clip — the UI gates, but a stale projection can race) only log.
fn set_param_keyframe_and_publish(
    engine: &mut Engine,
    clip: &str,
    param: ClipParam,
    at: RationalTime,
    value: ParamValue,
    easing: Easing,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-param-keyframe ignored: unparsable clip id");
        return;
    };
    match engine.apply(Command::Edit(EditCommand::SetParamKeyframe {
        clip: clip_id,
        param,
        at,
        value,
        easing,
    })) {
        Ok(_) => {
            info!(%clip_id, ?param, tick = at.value, "set param keyframe");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%clip_id, ?param, "set param keyframe failed: {e}"),
    }
}

/// Remove the keyframe at exactly `at` on one property (inspector diamond
/// toggled off). The engine rejects when nothing sits there.
fn remove_param_keyframe_and_publish(
    engine: &mut Engine,
    clip: &str,
    param: ClipParam,
    at: RationalTime,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "remove-param-keyframe ignored: unparsable clip id");
        return;
    };
    match engine.apply(Command::Edit(EditCommand::RemoveParamKeyframe {
        clip: clip_id,
        param,
        at,
    })) {
        Ok(_) => {
            info!(%clip_id, ?param, tick = at.value, "removed param keyframe");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%clip_id, ?param, "remove param keyframe failed: {e}"),
    }
}

/// Every animated property with a keyframe exactly at the clip-relative
/// `rel_tick`, with that keyframe's value and easing — the slice of one
/// merged timeline diamond (the timeline draws one diamond per tick across
/// all properties, CapCut-style).
fn keyframes_at(
    transform: &AnimatedTransform,
    rel_tick: i64,
) -> Vec<(ClipParam, ParamValue, Easing)> {
    let mut hits = Vec::new();
    if let Some(kf) = transform
        .position
        .keyframes()
        .iter()
        .find(|k| k.tick == rel_tick)
    {
        hits.push((ClipParam::Position, ParamValue::Vec2(kf.value), kf.easing));
    }
    let scalars = [
        (ClipParam::Scale, &transform.scale),
        (ClipParam::Rotation, &transform.rotation),
        (ClipParam::Opacity, &transform.opacity),
    ];
    for (param, p) in scalars {
        if let Some(kf) = p.keyframes().iter().find(|k| k.tick == rel_tick) {
            hits.push((param, ParamValue::Scalar(kf.value), kf.easing));
        }
    }
    hits
}

/// Move every keyframe at `from_tick` to `to_tick` (timeline diamond drag,
/// keyframes roadmap Phase 2): per property a remove + re-set with the same
/// value and easing, all in one history group so a single undo puts the
/// diamond back. A keyframe already sitting at the destination on the same
/// property is replaced (the diamonds merge, like CapCut). The engine
/// re-validates that `to_tick` falls inside the clip.
fn retime_keyframes_and_publish(
    engine: &mut Engine,
    clip: &str,
    from_tick: i64,
    to_tick: i64,
    tl_rate: Rational,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "retime-keyframes ignored: unparsable clip id");
        return;
    };
    if from_tick == to_tick {
        return;
    }
    let Some(model) = engine.project().clip(clip_id) else {
        error!(%clip_id, "retime-keyframes ignored: clip not on the timeline");
        return;
    };
    let moved = keyframes_at(&model.transform, from_tick - model.timeline.start.value);
    if moved.is_empty() {
        error!(%clip_id, from_tick, "retime-keyframes ignored: no keyframes at tick");
        return;
    }

    engine.begin_group();
    for (param, value, easing) in moved {
        if let Err(e) = engine.apply(Command::Edit(EditCommand::RemoveParamKeyframe {
            clip: clip_id,
            param,
            at: RationalTime::new(from_tick, tl_rate),
        })) {
            error!(%clip_id, ?param, "retime keyframes failed removing: {e}");
            engine.rollback_group();
            return;
        }
        if let Err(e) = engine.apply(Command::Edit(EditCommand::SetParamKeyframe {
            clip: clip_id,
            param,
            at: RationalTime::new(to_tick, tl_rate),
            value,
            easing,
        })) {
            error!(%clip_id, ?param, "retime keyframes failed setting: {e}");
            engine.rollback_group();
            return;
        }
    }
    engine.commit_group();
    info!(%clip_id, from_tick, to_tick, "retimed keyframes");
    publish_projection(engine, ui);
}

/// Remove every property's keyframe at `tick` (timeline diamond
/// right-click) as one history group — one undo restores the whole merged
/// diamond.
fn remove_keyframes_at_and_publish(
    engine: &mut Engine,
    clip: &str,
    tick: i64,
    tl_rate: Rational,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "remove-keyframes ignored: unparsable clip id");
        return;
    };
    let Some(model) = engine.project().clip(clip_id) else {
        error!(%clip_id, "remove-keyframes ignored: clip not on the timeline");
        return;
    };
    let hits = keyframes_at(&model.transform, tick - model.timeline.start.value);
    if hits.is_empty() {
        error!(%clip_id, tick, "remove-keyframes ignored: no keyframes at tick");
        return;
    }

    engine.begin_group();
    for (param, _, _) in hits {
        if let Err(e) = engine.apply(Command::Edit(EditCommand::RemoveParamKeyframe {
            clip: clip_id,
            param,
            at: RationalTime::new(tick, tl_rate),
        })) {
            error!(%clip_id, ?param, "remove keyframes failed: {e}");
            engine.rollback_group();
            return;
        }
    }
    engine.commit_group();
    info!(%clip_id, tick, "removed keyframes at tick");
    publish_projection(engine, ui);
}

// PORT: main warmed the disk cache here with a `prefetch_ahead` read-ahead
// after idle frames; this branch's engine keeps decode read-ahead internal
// to its native decoders, so the worker sends nothing.

fn import_and_publish(engine: &mut Engine, path: &Path, ui: &UiSink) {
    let _ = import_media_rpc_and_publish(engine, path, Some(ui));
}

/// Shared import implementation for fire-and-forget UI work and acknowledged
/// RPCs. `ui` is `None` only in engine-level unit tests.
fn import_media_rpc_and_publish(
    engine: &mut Engine,
    path: &Path,
    ui: Option<&UiSink>,
) -> Result<ImportMediaRpcResult, String> {
    match engine.apply(Command::Project(ProjectCommand::Import {
        path: path.to_path_buf(),
    })) {
        Ok(ApplyOutcome::Imported { media }) => {
            info!(
                ?media,
                path = %path.display(),
                pool = engine.project().media_count(),
                "imported media into pool"
            );
            // Kick off tile thumbnail generation off-thread; the tile shows
            // its placeholder until the image lands (see src/thumbnails.rs).
            let current_path = engine
                .project()
                .media(media)
                .map(|source| {
                    if let Some(ui) = ui {
                        register_media_with_workers(source, ui);
                    }
                    source.path().to_path_buf()
                })
                .ok_or_else(|| {
                    format!(
                        "import succeeded for {} but media {} is missing from the pool",
                        path.display(),
                        media.raw()
                    )
                })?;
            if let Some(ui) = ui {
                publish_projection(engine, ui);
            }
            Ok(ImportMediaRpcResult {
                media_id: media.raw(),
                path: current_path,
            })
        }
        Ok(other) => {
            let message = format!(
                "unexpected import outcome for {}: {other:?}",
                path.display()
            );
            error!("{message}");
            Err(message)
        }
        Err(e) => {
            let message = format!("import failed for {}: {e}", path.display());
            error!("{message}");
            Err(message)
        }
    }
}

/// One pool entry imported by an OS drop, ready to place: id, full source
/// range, and that range resampled to timeline ticks (what the clip will
/// occupy).
struct DroppedMedia {
    media: MediaId,
    source: TimeRange,
    duration_ticks: i64,
}

/// OS files dropped on the timeline (Finder / Explorer): import every path
/// and place the results end-to-end from the drop point — videos and images
/// on a video lane, audio-only files on an audio lane (the model's lane
/// zones keep audio at the bottom) — as **one undo group**, mobile
/// `append_main`-style. Without `target` the drop missed the timeline and
/// each path takes the plain pool-import path (today's behavior).
///
/// A file whose import/probe fails is skipped without aborting the rest;
/// the group commits with whatever landed.
fn drop_files_and_publish(
    engine: &mut Engine,
    paths: &[PathBuf],
    target: Option<(i64, i64)>,
    main_magnet: bool,
    ui: &UiSink,
) {
    let Some((drop_row, drop_tick)) = target else {
        for path in paths {
            import_and_publish(engine, path, ui);
        }
        return;
    };
    let tl_rate = engine.project().timeline().frame_rate;

    engine.begin_group();
    // Import first (inside the group, so one undo also clears the pool
    // entries this gesture added), classifying per landing lane kind. Order
    // within each kind is the order the OS delivered the files.
    let mut imported: Vec<MediaId> = Vec::new();
    let mut visual: Vec<DroppedMedia> = Vec::new();
    let mut audio: Vec<DroppedMedia> = Vec::new();
    for path in paths {
        match engine.apply(Command::Project(ProjectCommand::Import {
            path: path.clone(),
        })) {
            Ok(ApplyOutcome::Imported { media }) => {
                let Some(entry) = engine.project().media(media) else {
                    continue;
                };
                let source = entry.full_range();
                let dropped = DroppedMedia {
                    media,
                    source,
                    // Mirror Project::add_clip's source→timeline resampling
                    // so the plan sees the same extent the engine validates.
                    duration_ticks: resample(source.duration, tl_rate).value.max(1),
                };
                if entry.is_audio_only() {
                    audio.push(dropped);
                } else {
                    visual.push(dropped);
                }
                imported.push(media);
            }
            Ok(other) => {
                error!(path = %path.display(), "unexpected drop-import outcome: {other:?}");
            }
            Err(e) => error!(path = %path.display(), "drop import failed, file skipped: {e}"),
        }
    }

    place_drop_group(
        engine,
        &visual,
        TrackKind::Video,
        drop_row,
        drop_tick,
        main_magnet,
    );
    place_drop_group(engine, &audio, TrackKind::Audio, drop_row, drop_tick, false);

    // Commit whatever landed (an empty group is a no-op); pool bookkeeping
    // and the projection republish mirror import_and_publish.
    engine.commit_group();
    info!(
        files = paths.len(),
        placed = visual.len() + audio.len(),
        drop_row,
        drop_tick,
        "placed OS file drop on the timeline"
    );
    for media in imported {
        if let Some(source) = engine.project().media(media) {
            register_media_with_workers(source, ui);
        }
    }
    publish_projection(engine, ui);
}

/// Place one kind-group of an OS drop end-to-end. The landing lane mirrors
/// the library-drop policy (`add_clip_and_publish`): the lane of `kind` at
/// the drop row, else — for video — the *empty* main track (CapCut: video
/// dropped anywhere lands on the empty main lane), else a fresh lane
/// inserted at the drop row (lane zones then clamp it: audio sinks below the
/// main track). On the main lane with the magnet on, files ripple-insert at
/// the caret boundary; otherwise the chain first-fit slides past existing
/// clips. Failures skip the file, never the group.
fn place_drop_group(
    engine: &mut Engine,
    items: &[DroppedMedia],
    kind: TrackKind,
    drop_row: i64,
    drop_tick: i64,
    main_magnet: bool,
) {
    if items.is_empty() {
        return;
    }
    let tl_rate = engine.project().timeline().frame_rate;
    let lane = track_at_row(engine, drop_row)
        .filter(|t| t.kind == kind && !t.locked)
        .map(|t| t.id)
        .or_else(|| empty_main_lane(engine, kind));
    let lane = match lane {
        Some(id) => id,
        None => match create_track(engine, kind, drop_row) {
            Ok(id) => id,
            Err(e) => {
                error!("drop failed creating {kind:?} track: {e}");
                return;
            }
        },
    };

    let timeline = engine.project().timeline();
    let spans = timeline.track(lane).map(occupied_spans).unwrap_or_default();
    let insert = main_magnet && kind == TrackKind::Video && timeline.main_track() == Some(lane);
    let durations: Vec<i64> = items.iter().map(|m| m.duration_ticks).collect();
    let starts: Vec<i64> = if insert {
        // Magnet insert: chain from the caret boundary; every RippleInsert
        // shifts later clips right, so each next file lands at the previous
        // one's end.
        let boundary = crate::os_drop::insertion_boundary(&spans, drop_tick);
        durations
            .iter()
            .scan(boundary, |at, d| {
                let start = *at;
                *at += d;
                Some(start)
            })
            .collect()
    } else {
        crate::os_drop::plan_sequential_starts(&spans, drop_tick, &durations)
    };

    for (item, start) in items.iter().zip(starts) {
        let command = if insert {
            EditCommand::RippleInsert {
                track: lane,
                media: item.media,
                source: item.source,
                at: RationalTime::new(start, tl_rate),
            }
        } else {
            EditCommand::AddClip {
                track: lane,
                media: item.media,
                source: item.source,
                start: RationalTime::new(start, tl_rate),
            }
        };
        match engine.apply(Command::Edit(command)) {
            Ok(ApplyOutcome::Edited(EditOutcome::Created(clip))) => {
                info!(%clip, %lane, media = %item.media, start, insert, "placed dropped file");
            }
            Ok(other) => {
                error!(media = %item.media, "unexpected drop placement outcome: {other:?}");
            }
            Err(e) => error!(media = %item.media, %lane, start, "drop placement failed: {e}"),
        }
    }
}

/// The track at UI lane-list row `row` (top-first), if any. The engine
/// stacks bottom→top while the lane list renders top-first (projection.rs),
/// so row r ↔ stack index (count − 1 − r).
fn track_at_row(engine: &Engine, row: i64) -> Option<&Track> {
    let timeline = engine.project().timeline();
    let count = timeline.order().len() as i64;
    if !(0..count).contains(&row) {
        return None;
    }
    let id = timeline.order()[(count - 1 - row) as usize];
    timeline.track(id)
}

/// Sorted, non-overlapping `[start, end)` tick spans of every clip on
/// `track` — the occupancy input to the OS-drop placement planner.
fn occupied_spans(track: &Track) -> Vec<(i64, i64)> {
    track
        .clips_ordered()
        .iter()
        .map(|c| (c.timeline.start.value, c.timeline.end_tick()))
        .collect()
}

/// Persist the session to its draft file. `path` is the draft's
/// `project.cutlass` (binding a freshly created draft); `None` reuses the
/// engine's current path — the debounced auto-save and the flush before a
/// session swap / close. Success refreshes the draft's name sidecar and
/// republishes the projection; failure surfaces through `session-error`. A
/// `None` flush with no bound draft (e.g. New from the launch screen over the
/// empty boot session) has nothing to persist and is a quiet no-op.
fn save_project_and_publish(engine: &mut Engine, path: Option<PathBuf>, ui: &UiSink) {
    let _ = save_project_rpc_and_publish(engine, path, Some(ui));
}

/// Shared save implementation for fire-and-forget UI work and acknowledged
/// RPCs. A missing implicit path remains a quiet UI no-op, but is an explicit
/// RPC error. `ui` is `None` only in engine-level unit tests.
fn save_project_rpc_and_publish(
    engine: &mut Engine,
    path: Option<PathBuf>,
    ui: Option<&UiSink>,
) -> Result<SaveProjectRpcResult, String> {
    let Some(path) = path.or_else(|| engine.project_path().cloned()) else {
        return Err("save project failed: no current project path is bound".into());
    };
    match engine.apply(Command::Project(ProjectCommand::Save {
        path: path.clone(),
    })) {
        Ok(ApplyOutcome::Saved) => {
            crate::drafts::write_meta(&path, &engine.project().name);
            if let Some(ui) = ui {
                publish_projection(engine, ui);
            }
            let actual_path = engine.project_path().cloned().ok_or_else(|| {
                format!(
                    "save reported success for {} but the engine has no current project path",
                    path.display()
                )
            })?;
            if engine.is_dirty() {
                return Err(format!(
                    "save reported success for {} but the engine remains dirty",
                    actual_path.display()
                ));
            }
            Ok(SaveProjectRpcResult {
                path: actual_path,
                dirty: false,
            })
        }
        Ok(other) => {
            let message = format!("unexpected save outcome for {}: {other:?}", path.display());
            error!("{message}");
            Err(message)
        }
        Err(e) => {
            let message = format!("save failed for {}: {e}", path.display());
            error!("{message}");
            if let Some(ui) = ui {
                publish_session_error(
                    ui,
                    format!("Couldn't save the project to {}: {e}", path.display()),
                );
            }
            Err(message)
        }
    }
}

/// Internal error contract for a whole-session operation. The worker transport
/// gets `rpc_message`; the fire-and-forget UI transport gets `ui_message`.
/// `session_replaced_in_memory` distinguishes a rejected atomic operation from
/// a post-replacement persistence/binding failure that still needs projection
/// publication and an epoch bump.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionReplacementError {
    rpc_message: String,
    ui_message: String,
    session_replaced_in_memory: bool,
}

impl SessionReplacementError {
    fn unchanged(rpc_message: String, ui_message: String) -> Self {
        Self {
            rpc_message,
            ui_message,
            session_replaced_in_memory: false,
        }
    }

    fn after_replacement(rpc_message: String, ui_message: String) -> Self {
        Self {
            rpc_message,
            ui_message,
            session_replaced_in_memory: true,
        }
    }
}

/// An `Ok` session operation always replaced the session. An error only did
/// so when it occurred after the atomic engine replacement (currently draft
/// creation/binding after a template apply).
fn session_was_replaced<T>(result: &Result<T, SessionReplacementError>) -> bool {
    result
        .as_ref()
        .map(|_| true)
        .unwrap_or_else(|error| error.session_replaced_in_memory)
}

fn count_missing_media(engine: &Engine) -> usize {
    engine
        .project()
        .media_iter()
        .filter(|media| !media.path().exists())
        .count()
}

/// Finish one transport-neutral session operation. UI fire-and-forget callers
/// opt into one `session-error`; acknowledged RPC callers receive the explicit
/// error instead. A replaced in-memory session is published and bumps the
/// epoch exactly once even when its subsequent template binding failed.
fn complete_session_replacement<T>(
    engine: &mut Engine,
    ui: Option<&UiSink>,
    result: Result<T, SessionReplacementError>,
    publish_ui_error: bool,
) -> Result<T, String> {
    if publish_ui_error && let (Some(ui), Err(error)) = (ui, &result) {
        publish_session_error(ui, error.ui_message.clone());
    }

    if session_was_replaced(&result)
        && let Some(ui) = ui
    {
        for media in engine.project().media_iter() {
            if media.path().exists() {
                register_media_with_workers(media, ui);
            }
        }
        publish_projection(engine, ui);
        bump_session_epoch(ui);
    }

    result.map_err(|error| error.rpc_message)
}

/// Transport-neutral tolerant project load. Missing media is retained for the
/// relink flow, and the result reads the actual binding back from the engine.
fn open_project_core(
    engine: &mut Engine,
    path: PathBuf,
) -> Result<OpenProjectRpcResult, SessionReplacementError> {
    match engine.apply(Command::Project(ProjectCommand::Load {
        path: path.clone(),
    })) {
        Ok(ApplyOutcome::Loaded) => {
            let Some(actual_path) = engine.project_path().cloned() else {
                let message = format!(
                    "open project outcome uncertain/partially committed: {} replaced the \
                     in-memory session, but the engine reports no bound project path",
                    path.display()
                );
                error!("{message}");
                return Err(SessionReplacementError::after_replacement(
                    message,
                    "The project was opened, but its file binding couldn't be confirmed.".into(),
                ));
            };
            info!(
                path = %actual_path.display(),
                pool = engine.project().media_count(),
                "opened project"
            );
            Ok(OpenProjectRpcResult {
                path: actual_path,
                project_name: engine.project().name.clone(),
                missing_media_count: count_missing_media(engine),
            })
        }
        Ok(other) => {
            let message = format!("unexpected open outcome for {}: {other:?}", path.display());
            error!("{message}");
            Err(SessionReplacementError::unchanged(
                message,
                format!(
                    "Couldn't open {}: unexpected engine outcome {other:?}",
                    path.display()
                ),
            ))
        }
        Err(e) => {
            let message = format!("open project failed for {}: {e}", path.display());
            error!("{message}");
            Err(SessionReplacementError::unchanged(
                message,
                format!("Couldn't open {}: {e}", path.display()),
            ))
        }
    }
}

/// Fire-and-forget UI wrapper: errors are published exactly once.
fn open_project_and_publish(engine: &mut Engine, path: PathBuf, ui: &UiSink) {
    let result = open_project_core(engine, path);
    let _ = complete_session_replacement(engine, Some(ui), result, true);
}

/// Acknowledged wrapper: success still updates the live UI, while errors are
/// returned to the RPC caller and never duplicated through `session-error`.
fn open_project_rpc_and_publish(
    engine: &mut Engine,
    path: PathBuf,
    ui: Option<&UiSink>,
) -> Result<OpenProjectRpcResult, String> {
    let result = open_project_core(engine, path);
    complete_session_replacement(engine, ui, result, false)
}

/// Transport-neutral fresh-session replacement. It intentionally remains
/// unbound: the host owns app-draft creation and the subsequent queue-ordered
/// acknowledged save.
fn new_project_core(engine: &mut Engine) -> Result<NewProjectRpcResult, SessionReplacementError> {
    engine.new_session();
    info!("new session");
    let path = engine.project_path().cloned();
    Ok(NewProjectRpcResult {
        requires_save_binding: path.is_none(),
        path,
        project_name: engine.project().name.clone(),
        missing_media_count: count_missing_media(engine),
    })
}

fn new_project_and_publish(engine: &mut Engine, ui: &UiSink) {
    let result = new_project_core(engine);
    let _ = complete_session_replacement(engine, Some(ui), result, true);
}

fn new_project_rpc_and_publish(
    engine: &mut Engine,
    ui: Option<&UiSink>,
) -> Result<NewProjectRpcResult, String> {
    let result = new_project_core(engine);
    complete_session_replacement(engine, ui, result, false)
}

/// Transport-neutral template mutation and app-draft binding. The injected
/// creator is the production draft allocator and a filesystem-failure seam for
/// tests; it runs only after the engine atomically applied the template.
fn apply_template_core(
    engine: &mut Engine,
    path: PathBuf,
    picks: Vec<TemplatePick>,
    create_draft: impl FnOnce() -> std::io::Result<PathBuf>,
) -> Result<ApplyTemplateRpcResult, SessionReplacementError> {
    match engine.apply(Command::Project(ProjectCommand::ApplyTemplate {
        path: path.clone(),
        picks,
    })) {
        Ok(ApplyOutcome::AppliedTemplate) => {
            info!(
                template = %path.display(),
                pool = engine.project().media_count(),
                "applied template"
            );
        }
        Ok(other) => {
            let message = format!(
                "unexpected apply-template outcome for {}: {other:?}",
                path.display()
            );
            error!("{message}");
            return Err(SessionReplacementError::unchanged(
                message,
                format!("Couldn't use the template: unexpected engine outcome {other:?}"),
            ));
        }
        Err(e) => {
            let message = format!("apply template failed for {}: {e}", path.display());
            error!("{message}");
            return Err(SessionReplacementError::unchanged(
                message,
                format!("Couldn't use the template: {e}"),
            ));
        }
    }

    let draft = create_draft().map_err(|error| {
        let message = format!(
            "apply template outcome uncertain/partially committed: {} replaced the in-memory \
             session, but creating an app-owned draft failed: {error}; the current session is \
             unbound and has not been persisted",
            path.display()
        );
        error!("{message}");
        SessionReplacementError::after_replacement(
            message,
            format!("The template was applied but a project draft couldn't be created: {error}"),
        )
    })?;

    let saved =
        save_project_rpc_and_publish(engine, Some(draft.clone()), None).map_err(|error| {
            let binding = engine
                .project_path()
                .map(|bound| bound.display().to_string())
                .unwrap_or_else(|| "<unbound>".into());
            let message = format!(
                "apply template outcome uncertain/partially committed: {} replaced the in-memory \
             session, but binding/persisting app-owned draft {} failed or could not be verified: \
             {error}; current engine binding: {binding}",
                path.display(),
                draft.display()
            );
            error!("{message}");
            SessionReplacementError::after_replacement(
                message,
                format!(
                    "The template was applied but its project draft couldn't be saved: {error}"
                ),
            )
        })?;

    Ok(ApplyTemplateRpcResult {
        path: saved.path,
        project_name: engine.project().name.clone(),
        missing_media_count: count_missing_media(engine),
    })
}

fn apply_template_and_publish(
    engine: &mut Engine,
    path: PathBuf,
    picks: Vec<TemplatePick>,
    ui: &UiSink,
) {
    let result = apply_template_core(engine, path, picks, crate::drafts::create);
    let _ = complete_session_replacement(engine, Some(ui), result, true);
}

fn apply_template_rpc_and_publish(
    engine: &mut Engine,
    path: PathBuf,
    picks: Vec<TemplatePick>,
    ui: Option<&UiSink>,
) -> Result<ApplyTemplateRpcResult, String> {
    let result = apply_template_core(engine, path, picks, crate::drafts::create);
    complete_session_replacement(engine, ui, result, false)
}

/// Re-point a pool entry at a user-picked file (missing-media relink, M0).
/// The engine re-probes and swaps the entry's path/metadata in place (same
/// id — clips recover without being touched); the tile workers re-register
/// so the thumbnail and filmstrips regenerate from the new file; the
/// projection republish clears the entry's missing badge and decrements
/// the dialog's count. Failures (unreadable file, probe error) surface
/// through `session-error` and leave the entry untouched.
fn relink_media_and_publish(engine: &mut Engine, media: &str, path: &Path, ui: &UiSink) {
    let _ = relink_media_rpc_and_publish(engine, media, path, Some(ui));
}

/// Shared single-media relink implementation for UI work and acknowledged
/// RPCs. `ui` is `None` only in engine-level unit tests.
fn relink_media_rpc_and_publish(
    engine: &mut Engine,
    media: &str,
    path: &Path,
    ui: Option<&UiSink>,
) -> Result<RelinkMediaRpcResult, String> {
    let Some(media_id) = parse_raw_id(media).map(MediaId::from_raw) else {
        let message = format!("relink failed: unparsable media id `{media}`");
        error!("{message}");
        return Err(message);
    };
    match engine.apply(Command::Project(ProjectCommand::RelinkMedia {
        media: media_id,
        path: path.to_path_buf(),
    })) {
        Ok(ApplyOutcome::Relinked { media: relinked }) => {
            if relinked != media_id {
                let message = format!(
                    "relink for media {} returned mismatched media {}",
                    media_id.raw(),
                    relinked.raw()
                );
                error!("{message}");
                return Err(message);
            }
            info!(?relinked, path = %path.display(), "relinked media");
            let current_path = engine
                .project()
                .media(relinked)
                .map(|source| {
                    if let Some(ui) = ui {
                        register_media_with_workers(source, ui);
                    }
                    source.path().to_path_buf()
                })
                .ok_or_else(|| {
                    format!(
                        "relink succeeded but media {} is missing from the pool",
                        relinked.raw()
                    )
                })?;
            if let Some(ui) = ui {
                publish_projection(engine, ui);
            }
            Ok(RelinkMediaRpcResult {
                media_id: relinked.raw(),
                path: current_path,
            })
        }
        Ok(other) => {
            let message = format!(
                "unexpected relink outcome for media {} to {}: {other:?}",
                media_id.raw(),
                path.display()
            );
            error!("{message}");
            Err(message)
        }
        Err(e) => {
            let message = format!(
                "relink failed for media {} to {}: {e}",
                media_id.raw(),
                path.display()
            );
            error!("{message}");
            if let Some(ui) = ui {
                publish_session_error(ui, format!("Couldn't relink to {}: {e}", path.display()));
            }
            Err(message)
        }
    }
}

/// Try `folder/<filename>` for every missing pool entry; relink each match.
fn relink_folder_and_publish(engine: &mut Engine, folder: PathBuf, ui: &UiSink) {
    let result = relink_folder_rpc_and_publish(engine, folder, Some(ui));
    report_relink_folder_error(&result, |message| publish_session_error(ui, message));
}

/// Invoke `report` exactly once for a failed UI folder relink and never for
/// success. Keeping this decision outside the shared operation gives errors a
/// single owner: fire-and-forget UI calls publish `session-error`, while RPC
/// calls return the same string to their caller without a duplicate dialog.
fn report_relink_folder_error(
    result: &Result<RelinkFolderRpcResult, String>,
    report: impl FnOnce(String),
) {
    if let Err(message) = result {
        report(message.clone());
    }
}

/// Shared folder-relink operation for UI work and acknowledged RPCs.
///
/// `RelinkMedia` is intentionally non-undoable, so an engine history group
/// cannot make this operation atomic. Every candidate is canonicalized and
/// probed before the first mutation. The engine then re-probes while applying
/// each relink; a filesystem race can still make an individual apply fail.
/// In that case all remaining candidates are attempted, successful relinks
/// stay applied, the UI is published once, and the RPC returns an explicit
/// partial-failure error naming the retained successes. Error reporting is
/// deliberately transport-neutral: this function logs and returns errors but
/// never publishes `session-error`. `ui` supplies success-side worker
/// registration/projection effects and is `None` only in engine-level tests.
fn relink_folder_rpc_and_publish(
    engine: &mut Engine,
    folder: PathBuf,
    ui: Option<&UiSink>,
) -> Result<RelinkFolderRpcResult, String> {
    let mut candidates: Vec<(MediaId, PathBuf)> = engine
        .project()
        .media_iter()
        .filter(|media| !media.path().exists())
        .filter_map(|media| {
            media
                .path()
                .file_name()
                .map(|name| (media.id, folder.join(name)))
        })
        .filter(|(_, candidate)| candidate.exists())
        .collect();
    candidates.sort_by_key(|(media, _)| media.raw());

    if candidates.is_empty() {
        let message = format!(
            "No missing media files were found in {}. \
             Pick individual files or choose a folder that contains them.",
            folder.display()
        );
        return Err(message);
    }

    // Validate every candidate before the first non-undoable mutation. This
    // is the same native probe RelinkMedia performs after canonicalization.
    for (media, path) in &mut candidates {
        let canonical = path.canonicalize().map_err(|e| {
            let message = format!(
                "folder relink preflight failed for media {} at {}: {e}; no media was relinked",
                media.raw(),
                path.display()
            );
            error!("{message}");
            message
        })?;
        cutlass_decoder::probe(&canonical).map_err(|e| {
            let message = format!(
                "folder relink preflight failed for media {} at {}: {e}; no media was relinked",
                media.raw(),
                canonical.display()
            );
            error!("{message}");
            message
        })?;
        *path = canonical;
    }

    let mut relinked = Vec::with_capacity(candidates.len());
    let mut failures = Vec::new();
    for (media_id, path) in candidates {
        match engine.apply(Command::Project(ProjectCommand::RelinkMedia {
            media: media_id,
            path: path.clone(),
        })) {
            Ok(ApplyOutcome::Relinked { media }) => match engine.project().media(media) {
                Some(source) => {
                    if let Some(ui) = ui {
                        register_media_with_workers(source, ui);
                    }
                    relinked.push(RelinkMediaRpcResult {
                        media_id: media.raw(),
                        path: source.path().to_path_buf(),
                    });
                }
                None => {
                    let message = format!(
                        "media {} disappeared after a successful relink to {}",
                        media.raw(),
                        path.display()
                    );
                    error!("{message}");
                    failures.push(message);
                }
            },
            Ok(other) => {
                let message = format!(
                    "unexpected folder relink outcome for media {} at {}: {other:?}",
                    media_id.raw(),
                    path.display()
                );
                error!("{message}");
                failures.push(message);
            }
            Err(e) => {
                let message = format!(
                    "folder relink failed for media {} at {}: {e}",
                    media_id.raw(),
                    path.display()
                );
                error!("{message}");
                failures.push(message);
            }
        }
    }

    relinked.sort_by_key(|entry| entry.media_id);
    if !relinked.is_empty() {
        info!(count = relinked.len(), folder = %folder.display(), "relinked media from folder");
        if let Some(ui) = ui {
            publish_projection(engine, ui);
        }
    }

    if !failures.is_empty() {
        let retained = relinked
            .iter()
            .map(|entry| entry.media_id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let partial = if relinked.is_empty() {
            "no relinks succeeded".to_string()
        } else {
            format!("non-undoable successful relinks remain applied for media ids [{retained}]")
        };
        return Err(format!(
            "folder relink completed with individual failures; {partial}; {}",
            failures.join("; ")
        ));
    }

    Ok(RelinkFolderRpcResult { relinked })
}

/// Delete a source from the media pool (Library bin). `force` false removes
/// only unreferenced media — the engine rejects a referenced source, which the
/// UI prevents by gating on the tile's usage count. `force` true first deletes
/// every clip referencing the source and then the source, all in one history
/// group, so a single undo restores both. The thumbnail cache entry is evicted
/// on success. Lanes the cascade empties are pruned, matching the clip-delete
/// flow (`remove_clips_and_publish`).
fn remove_media_and_publish(engine: &mut Engine, media: &str, force: bool, ui: &UiSink) {
    let Some(media_id) = parse_raw_id(media).map(MediaId::from_raw) else {
        error!(media, "delete-media ignored: unparsable media id");
        return;
    };
    if engine.project().media(media_id).is_none() {
        error!(%media_id, "delete-media ignored: not in the pool");
        return;
    }

    if !force {
        match engine.apply(Command::Project(ProjectCommand::RemoveMedia {
            media: media_id,
        })) {
            Ok(ApplyOutcome::RemovedMedia { media }) => {
                info!(?media, "removed media from pool");
                crate::thumbnails::forget(media.raw());
                publish_projection(engine, ui);
            }
            Ok(other) => error!(%media_id, "unexpected remove-media outcome: {other:?}"),
            // The UI only sends the unforced delete for an unreferenced tile,
            // so a rejection here is a race (a clip landed on it between the
            // projection and the click) — surface it instead of dropping it.
            Err(e) => {
                error!(%media_id, "remove media failed: {e}");
                publish_session_error(ui, format!("Couldn't remove the media: {e}"));
            }
        }
        return;
    }

    // Cascade: gather every clip that references the source up front (a Library
    // delete leaves gaps where the clips sat — it isn't a timeline-timing
    // edit), then remove them and the source as one undoable group.
    let mut doomed: Vec<(ClipId, TrackId)> = Vec::new();
    for track in engine.project().timeline().tracks_ordered() {
        for clip in track.clips() {
            if clip.media() == Some(media_id) {
                doomed.push((clip.id, track.id));
            }
        }
    }

    engine.begin_group();
    for &(clip_id, _) in &doomed {
        if let Err(e) = apply_edit(engine, EditCommand::RemoveClip { clip: clip_id }) {
            error!(%clip_id, "remove referencing clip failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    if let Err(e) = engine.apply(Command::Project(ProjectCommand::RemoveMedia {
        media: media_id,
    })) {
        error!(%media_id, "remove media failed after clearing clips: {e}");
        engine.rollback_group();
        publish_projection(engine, ui);
        return;
    }
    // Prune lanes the removals emptied (CapCut drops emptied overlay tracks).
    let mut lanes: Vec<TrackId> = doomed.iter().map(|&(_, track)| track).collect();
    lanes.sort();
    lanes.dedup();
    for lane in lanes {
        remove_track_if_empty(engine, lane);
    }
    engine.commit_group();
    info!(%media_id, clips = doomed.len(), "removed media and its referencing clips");
    crate::thumbnails::forget(media_id.raw());
    publish_projection(engine, ui);
}

/// Register one pool media with the off-thread tile workers: a library
/// thumbnail render and the strip worker's id → path record (filmstrips /
/// waveforms resolve by media id alone). Shared by import, open, and relink.
fn register_media_with_workers(media: &cutlass_models::MediaSource, ui: &UiSink) {
    let kind = match media.kind() {
        cutlass_models::MediaKind::Audio => ThumbKind::Audio,
        cutlass_models::MediaKind::Image => ThumbKind::Image,
        cutlass_models::MediaKind::Video => ThumbKind::Video,
    };
    ui.thumbs
        .request(media.id.raw(), media.path().to_path_buf(), kind);
    // Stills register too: the strip sampler repeats the one picture across
    // the clip's filmstrip tiles.
    ui.strips
        .register_media(media.id.raw(), media.path().to_path_buf());
    // Large video sources get a preview proxy encoded in the background
    // (or re-bound instantly when one is already on disk); the worker
    // skips sources small enough to decode comfortably.
    if media.kind() == cutlass_models::MediaKind::Video {
        ui.proxy.request(
            media.id.raw(),
            media.path().to_path_buf(),
            media.width,
            media.height,
        );
    }
}

/// One current media descriptor captured before proxy refresh mutates the
/// engine's renderer state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyRefreshMedia {
    id: MediaId,
    path: PathBuf,
    width: u32,
    height: u32,
    is_video: bool,
}

/// Snapshot the media catalog needed to clear and re-request proxy bindings.
///
/// Project media lives in a hash map, so sorting makes the refresh order
/// deterministic without changing its semantics.
fn plan_proxy_refresh(project: &Project) -> Vec<ProxyRefreshMedia> {
    let mut media: Vec<_> = project
        .media_iter()
        .map(|source| ProxyRefreshMedia {
            id: source.id,
            path: source.path().to_path_buf(),
            width: source.width,
            height: source.height,
            is_video: source.kind() == cutlass_models::MediaKind::Video,
        })
        .collect();
    media.sort_unstable_by_key(|source| source.id.raw());
    media
}

/// Rebind proxy-dependent runtime state after the proxy cache root moves.
///
/// This deliberately bypasses project commands: renderer substitutions,
/// strip scratch state, and delivered preview frames are session caches, so
/// refreshing them must not touch Project, history, or revision.
fn refresh_proxies_after_maintenance(engine: &mut Engine, cache: &FrameCache, ui: &UiSink) {
    // Collect every descriptor before the first mutable Engine call so no
    // project borrow can overlap renderer mutation.
    let media = plan_proxy_refresh(engine.project());

    for source in &media {
        engine.clear_media_proxy(source.id);
    }
    ui.strips.clear_proxies();
    cache.clear();

    for source in media.into_iter().filter(|source| source.is_video) {
        ui.proxy
            .request(source.id.raw(), source.path, source.width, source.height);
    }
}

/// Bind a finished preview proxy to its pool media — only while the pool
/// entry still names `source`, the file the job was keyed to (a relink or
/// session swap in flight makes the id stale; the registries the engine
/// clears on those paths must never be repopulated with old files). On a
/// match the engine decodes the proxy from the next frame; delivered
/// frames composited from the original are dropped so the repaint (owed
/// via [`mutation_redraws_preview`]) and everything after render through
/// the proxy, and the strip worker re-points future filmstrip decodes.
fn bind_media_proxy(
    engine: &mut Engine,
    media_id: u64,
    source: &Path,
    proxy: PathBuf,
    cache: &FrameCache,
    ui: &UiSink,
) {
    let media = MediaId::from_raw(media_id);
    match engine.project().media(media) {
        Some(m) if m.path() == source => {
            info!(%media, proxy = %proxy.display(), "preview proxy bound");
            engine.set_media_proxy(media, proxy.clone());
            cache.clear();
            ui.strips.register_proxy(media_id, proxy);
        }
        Some(_) => info!(%media, "proxy ignored: media was relinked while it generated"),
        None => info!(%media, "proxy ignored: media left the pool while it generated"),
    }
}

/// Rename the current draft (title bar). Applied as one undoable edit so it
/// joins the undo history and dirties the session; the projection republish
/// updates the title, and the debounced auto-save writes the new name into
/// the draft's project file and meta sidecar.
fn rename_project_and_publish(engine: &mut Engine, name: String, ui: &UiSink) {
    // The title field commits on blur as well as Enter, so a focus-and-leave
    // with no change arrives here unchanged — skip it so it never spends an
    // undo entry or dirties the draft.
    if engine.project().name == name {
        return;
    }
    match engine.apply(Command::Edit(EditCommand::SetProjectName { name })) {
        Ok(_) => publish_projection(engine, ui),
        Err(e) => error!("project rename failed: {e}"),
    }
}

/// Place the full source range of `media` on a video track (audio-only media
/// lands on an audio track), then republish the projection so the clip appears.
///
/// Placement policy (CapCut-ish):
/// - dropped on a lane of the media's kind → that lane, sliding right into the
///   first gap that fits when the drop tick overlaps existing clips;
/// - dropped on empty timeline space (`track` empty) or a lane of another
///   kind → a fresh track of the media's kind inserted at `drop_row`, so the
///   new lane appears where the user dropped (above the lanes ⇒ top of the
///   stack, below ⇒ bottom);
/// - dropped on the main lane with the magnet on (`insert`) → ripple-insert
///   at `start_tick`, shifting later clips right (atomic engine command).
///
/// A video drop whose media carries audio lands a *single* clip — CapCut keeps
/// the sound on the video clip and the audio mixers read it from that lane, so
/// no separate audio lane is spawned.
#[allow(clippy::too_many_arguments)]
fn add_clip_and_publish(
    engine: &mut Engine,
    media: &str,
    track: &str,
    start_tick: i64,
    drop_row: i64,
    insert: bool,
    ui: &UiSink,
) {
    let Some(media_id) = parse_raw_id(media).map(MediaId::from_raw) else {
        error!(media, "drop ignored: unparsable media id");
        return;
    };
    let Some((source, audio_only)) = engine
        .project()
        .media(media_id)
        .map(|m| (m.full_range(), m.is_audio_only()))
    else {
        error!(%media_id, "drop ignored: media not in pool");
        return;
    };
    let lane_kind = if audio_only {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };
    let tl_rate = engine.project().timeline().frame_rate;
    // Mirror Project::add_clip's source→timeline resampling so first-fit sees
    // the same extent the engine will validate.
    let duration_ticks = resample(source.duration, tl_rate).value.max(1);

    // CapCut keeps a video's sound on the video clip itself — a drop lands one
    // clip and the audio mixers read its audio from that lane (see
    // `audio_snapshot`). No companion lane is spawned; use Extract audio
    // (`extract_audio_and_publish`) for the explicit detach gesture.

    // The main-track magnet only applies to the main *video* lane.
    if insert
        && !audio_only
        && let Some(lane) = lane_of_kind(engine, track, TrackKind::Video)
    {
        let at = start_tick.max(0);
        engine.begin_group();
        match engine.apply(Command::Edit(EditCommand::RippleInsert {
            track: lane,
            media: media_id,
            source,
            at: RationalTime::new(at, tl_rate),
        })) {
            Ok(ApplyOutcome::Edited(EditOutcome::Created(clip))) => {
                engine.commit_group();
                info!(%clip, %lane, %media_id, at, "ripple-inserted clip from library drop");
            }
            Ok(other) => {
                error!(%media_id, "unexpected ripple-insert outcome: {other:?}");
                engine.rollback_group();
            }
            Err(e) => {
                error!(%media_id, %lane, start_tick, "ripple insert failed: {e}");
                engine.rollback_group();
            }
        }
        publish_projection(engine, ui);
        return;
    }
    let desired = start_tick.max(0);

    // One history entry per drop, even when it creates the landing lane.
    // A video drop that misses every lane falls back to the empty main
    // track before creating an overlay lane (CapCut: the first video
    // dragged anywhere fills the main track).
    engine.begin_group();
    let (track_id, start_value) = match lane_of_kind(engine, track, lane_kind)
        .or_else(|| empty_main_lane(engine, lane_kind))
    {
        Some(lane) => {
            let lane_track = engine
                .project()
                .timeline()
                .track(lane)
                .expect("lane_of_kind returned an existing track");
            (lane, first_fit_start(lane_track, desired, duration_ticks))
        }
        None => match create_track(engine, lane_kind, drop_row) {
            Ok(id) => (id, desired),
            Err(e) => {
                error!(%media_id, "drop failed creating {lane_kind:?} track: {e}");
                engine.rollback_group();
                return;
            }
        },
    };

    match engine.apply(Command::Edit(EditCommand::AddClip {
        track: track_id,
        media: media_id,
        source,
        start: RationalTime::new(start_value, tl_rate),
    })) {
        Ok(ApplyOutcome::Edited(EditOutcome::Created(clip))) => {
            engine.commit_group();
            info!(
                %clip, %track_id, %media_id,
                start_tick = start_value,
                desired,
                "added clip from library drop"
            );
            publish_projection(engine, ui);
        }
        // First-fit should have made the placement valid; the engine still
        // rejects atomically if not. Surface the reason and roll the group
        // back so a lane created for this drop doesn't linger.
        Ok(other) => {
            error!(%media_id, "unexpected add-clip outcome: {other:?}");
            engine.rollback_group();
            publish_projection(engine, ui);
        }
        Err(e) => {
            error!(%media_id, %track_id, start_tick = start_value, "add clip failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
        }
    }
}

/// Place a generated clip (text/solid/shape/effect) from a library-tile
/// drop. One history entry, even when it creates the landing lane; rolled
/// back on a rejected placement so a lane made for the drop doesn't linger.
/// `effect` seeds the new clip's chain (standalone effect-lane segments);
/// `animations` attaches a text preset's look animations (unknown slots or
/// catalog ids are skipped with a warning — a served preset must not brick
/// the drop).
#[allow(clippy::too_many_arguments)]
fn add_generated_and_publish(
    engine: &mut Engine,
    generator: Generator,
    track: &str,
    start_tick: i64,
    duration_ticks: i64,
    drop_row: i64,
    effect: Option<&str>,
    animations: &[(String, String)],
    ui: &UiSink,
) {
    let Some(lane_kind) = TrackKind::for_generator(&generator) else {
        error!(
            ?generator,
            "generated drop ignored: no lane kind for generator"
        );
        return;
    };
    let desired = start_tick.max(0);
    let duration = duration_ticks.max(1);

    engine.begin_group();
    let track_id = match lane_of_kind(engine, track, lane_kind) {
        Some(lane) => {
            let lane_track = engine
                .project()
                .timeline()
                .track(lane)
                .expect("lane_of_kind returned an existing track");
            let start = first_fit_start(lane_track, desired, duration);
            (lane, start)
        }
        None => match create_track(engine, lane_kind, drop_row) {
            Ok(id) => (id, desired),
            Err(e) => {
                error!(
                    ?generator,
                    "generated drop failed creating {lane_kind:?} track: {e}"
                );
                engine.rollback_group();
                return;
            }
        },
    };
    let (track_id, start_value) = track_id;

    let content = ClipSource::Generated(generator);
    match add_clip_content(engine, track_id, &content, duration, start_value) {
        Ok(clip) => {
            // Effect drops carry the catalog effect onto the fresh segment's
            // chain, still inside the drop's history group.
            if let Some(effect_id) = effect
                && let Err(e) = engine.apply(Command::Edit(EditCommand::AddEffect {
                    clip,
                    effect_id: effect_id.to_string(),
                }))
            {
                error!(%clip, effect_id, "effect drop failed adding effect: {e}");
                engine.rollback_group();
                publish_projection(engine, ui);
                return;
            }
            // Text-preset animations ride the same history group. Skips
            // (unknown slot/id) and failures degrade to an unanimated title
            // rather than rejecting the drop.
            for (slot, animation_id) in animations {
                let Some(animation_slot) = parse_animation_slot(slot) else {
                    warn!(slot, "preset animation skipped: unknown slot");
                    continue;
                };
                if cutlass_models::animation_spec(animation_id).is_none() {
                    warn!(animation_id, "preset animation skipped: unknown catalog id");
                    continue;
                }
                if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipAnimation {
                    clip,
                    slot: animation_slot,
                    animation: Some(cutlass_models::AnimationRef::new(animation_id)),
                })) {
                    warn!(%clip, animation_id, "preset animation skipped: {e}");
                }
            }
            engine.commit_group();
            info!(%clip, %track_id, start_tick = start_value, "added generated clip from drop");
            publish_projection(engine, ui);
        }
        Err(e) => {
            error!(%track_id, start_tick = start_value, "add generated clip failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
        }
    }
}

/// Retime a media clip (CapCut speed, M1). The engine validates (positive
/// speed, media-backed clip, no neighbor overlap) and re-derives the
/// timeline duration; one undoable history entry. With linkage on, the
/// clip's link partners (the video+audio pair from one media drop) retime
/// together in one history group, so the pair stays in sync and one undo
/// restores both. Audio of retimed clips is muted by the snapshot builder,
/// so the republish silences it immediately.
fn set_clip_speed_and_publish(
    engine: &mut Engine,
    clip: &str,
    num: i32,
    den: i32,
    reversed: bool,
    linkage: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-clip-speed ignored: unparsable clip id");
        return;
    };
    let targets = if linkage {
        link_group_ids(engine, clip_id)
    } else {
        vec![clip_id]
    };

    engine.begin_group();
    for target in &targets {
        if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipSpeed {
            clip: *target,
            speed: Rational::new(num, den),
            reversed,
        })) {
            error!(clip_id = %target, "set clip speed failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    engine.commit_group();
    info!(%clip_id, num, den, reversed, clips = targets.len(), "retimed clip");
    publish_projection(engine, ui);
}

/// Toggle pitch preservation on a retimed media clip (CapCut "pitch" switch,
/// M8 Phase 3). With linkage on the whole link group flips together so an A/V
/// pair stays consistent — one undoable history entry. The republish
/// re-snapshots the mixer so the new stretch mode is audible immediately.
fn set_clip_pitch_and_publish(
    engine: &mut Engine,
    clip: &str,
    preserve: bool,
    linkage: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-clip-pitch ignored: unparsable clip id");
        return;
    };
    let targets = if linkage {
        link_group_ids(engine, clip_id)
    } else {
        vec![clip_id]
    };

    engine.begin_group();
    for target in &targets {
        if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipPitch {
            clip: *target,
            preserve_pitch: preserve,
        })) {
            error!(clip_id = %target, "set clip pitch failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    engine.commit_group();
    info!(%clip_id, preserve, clips = targets.len(), "set clip pitch");
    publish_projection(engine, ui);
}

/// Toggle noise reduction on a media clip (CapCut "Reduce noise", M8 Phase 5).
/// With linkage on, the whole link group follows so selecting a video half
/// still cleans its audio companion — one undoable history group. The
/// republish re-snapshots the mixer, which renders the cleaned signal.
fn set_denoise_and_publish(
    engine: &mut Engine,
    clip: &str,
    denoise: bool,
    linkage: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-denoise ignored: unparsable clip id");
        return;
    };
    let targets = if linkage {
        link_group_ids(engine, clip_id)
    } else {
        vec![clip_id]
    };

    engine.begin_group();
    for target in &targets {
        if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipDenoise {
            clip: *target,
            denoise,
        })) {
            error!(clip_id = %target, "set denoise failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    engine.commit_group();
    info!(%clip_id, denoise, clips = targets.len(), "set denoise");
    publish_projection(engine, ui);
}

/// Set (or clear) a media clip's speed ramp (CapCut speed curves, M2). Like
/// constant-speed retiming the engine re-derives each clip's timeline
/// duration from the ramp average, so with linkage on every link partner
/// ramps in lockstep to keep A/V in sync — one undoable history group. The
/// republish re-snapshots the mixer, which now plays the ramp time-stretched
/// along its curve (M8 Phase 3).
fn set_speed_curve_and_publish(
    engine: &mut Engine,
    clip: &str,
    curve: &Option<Param<f32>>,
    linkage: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-speed-curve ignored: unparsable clip id");
        return;
    };
    let targets = if linkage {
        link_group_ids(engine, clip_id)
    } else {
        vec![clip_id]
    };

    engine.begin_group();
    for target in &targets {
        if let Err(e) = engine.apply(Command::Edit(EditCommand::SetSpeedCurve {
            clip: *target,
            curve: curve.clone(),
        })) {
            error!(clip_id = %target, "set speed curve failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    engine.commit_group();
    info!(%clip_id, points = curve.as_ref().map_or(0, |c| c.keyframes().len()), clips = targets.len(), "set speed ramp");
    publish_projection(engine, ui);
}

/// Adjust one existing ramp point's multiplier (velocity-graph drag). Reads
/// the addressed clip's current curve, replaces point `index`'s value, and
/// re-commits through [`set_speed_curve_and_publish`] so duration re-derive,
/// linkage, and undo all flow through the one path.
fn set_speed_curve_point_and_publish(
    engine: &mut Engine,
    clip: &str,
    index: usize,
    value: f32,
    linkage: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-speed-curve-point ignored: unparsable clip id");
        return;
    };
    let Some(mut curve) = engine
        .project()
        .clip(clip_id)
        .map(|c| c.speed_curve.clone())
    else {
        error!(%clip_id, "set-speed-curve-point ignored: unknown clip");
        return;
    };
    // Address the point by index, but edit it through the keyframe API at its
    // own tick so the curve keeps its shape (tick + easing) and stays sorted.
    let Some(&point) = curve.keyframes().get(index) else {
        warn!(%clip_id, index, "set-speed-curve-point ignored: index out of range");
        return;
    };
    curve.set_keyframe(point.tick, value.clamp(MIN_SPEED, MAX_SPEED), point.easing);
    set_speed_curve_and_publish(engine, clip, &Some(curve), linkage, ui);
}

/// Set a clip's audio mix (CapCut volume + fades, M1). A video clip carries
/// its own sound, so the edit lands on the clicked clip; only when its audio
/// was detached to a linked audio lane does it follow to the audible half
/// there. One history group; the republish re-snapshots the playback mixer, so
/// the change is audible within a block.
fn set_clip_audio_and_publish(
    engine: &mut Engine,
    clip: &str,
    volume: Option<f32>,
    fade_in_s: f32,
    fade_out_s: f32,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-clip-audio ignored: unparsable clip id");
        return;
    };
    // CapCut keeps a video's sound on its own clip, so volume/fades land on the
    // clicked clip when it carries its own audio (a video drop, or an audio
    // lane). Only when its sound was detached to a linked audio lane does the
    // edit follow to the audible half there.
    let targets: Vec<ClipId> = if engine.project().timeline().carries_own_audio(clip_id) {
        vec![clip_id]
    } else {
        link_group_ids(engine, clip_id)
            .into_iter()
            .filter(|id| engine.project().timeline().carries_own_audio(*id))
            .collect()
    };
    if targets.is_empty() {
        warn!(%clip_id, "set-clip-audio ignored: no audible clip to adjust");
        return;
    }

    let tl_rate = engine.project().timeline().frame_rate;
    let to_ticks = |seconds: f32| {
        let ticks = (f64::from(seconds) * f64::from(tl_rate.num) / f64::from(tl_rate.den)).round();
        RationalTime::new(ticks.max(0.0) as i64, tl_rate)
    };
    let (fade_in, fade_out) = (to_ticks(fade_in_s), to_ticks(fade_out_s));

    engine.begin_group();
    for target in &targets {
        if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipAudio {
            clip: *target,
            volume,
            fade_in,
            fade_out,
        })) {
            error!(clip_id = %target, "set clip audio failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    engine.commit_group();
    info!(%clip_id, ?volume, fade_in_s, fade_out_s, clips = targets.len(), "set clip audio");
    publish_projection(engine, ui);
}

/// Duck a music clip under the voice lanes (M8 Phase 4). Gathers every clip on
/// a voice-tagged (`duck_source`) audio lane that overlaps the selected music
/// clip and lowers `DuckLanes` onto it — the engine writes the dip as ordinary
/// M8 volume keyframes, so the result is one undoable edit, audible on the next
/// mixer snapshot and editable through the volume envelope afterwards. The
/// defaults mirror the decoder's broadcast-typical ducker (and the agent
/// `duck` tool); the linear speech-band threshold stays an internal detail.
fn duck_under_voice_and_publish(engine: &mut Engine, clip: &str, ui: &UiSink) {
    let Some(music_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "duck-under-voice ignored: unparsable clip id");
        return;
    };

    // Resolve the overlapping voice clips against an immutable view, never
    // ducking a clip under its own lane.
    let voice: Vec<ClipId> = {
        let project = engine.project();
        let timeline = project.timeline();
        let Some(music) = project.clip(music_id) else {
            warn!(%music_id, "duck-under-voice ignored: unknown clip");
            return;
        };
        let music_track = timeline.track_of(music_id);
        let music_range = music.timeline;
        timeline
            .tracks_ordered()
            .filter(|track| {
                track.kind == TrackKind::Audio && track.duck_source && Some(track.id) != music_track
            })
            .flat_map(|track| track.clips_ordered())
            .filter(|c| c.timeline.overlaps(music_range).unwrap_or(false))
            .map(|c| c.id)
            .collect()
    };
    if voice.is_empty() {
        warn!(%music_id, "duck-under-voice: no voice-lane clips overlap the selected music");
        return;
    }

    match engine.apply(Command::Edit(EditCommand::DuckLanes {
        voice,
        music: vec![music_id],
        // Mirror `DuckSettings::default()` / the agent `duck` tool defaults.
        threshold: 0.025,
        amount: 0.66,
        attack: 0.08,
        release: 0.32,
    })) {
        Ok(ApplyOutcome::Edited(EditOutcome::Updated(_))) => {
            info!(%music_id, "ducked music under voice");
            publish_projection(engine, ui);
        }
        Ok(other) => error!(%music_id, "unexpected duck-under-voice outcome: {other:?}"),
        Err(e) => error!(%music_id, "duck under voice failed: {e}"),
    }
}

/// Detect beat markers on a media clip (CapCut "Beat", M8 Phase 6): the engine
/// decodes the clip's audio, runs onset/tempo analysis, and stores the beat
/// grid (source ticks) so the timeline magnet can snap clip edges to it. One
/// undoable history entry. A rejection (generated clip / no audio) just logs —
/// the inspector only offers the button on media clips with sound, so it would
/// be a stale-projection race.
fn detect_beats_and_publish(engine: &mut Engine, clip: &str, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "detect-beats ignored: unparsable clip id");
        return;
    };
    match engine.apply(Command::Edit(EditCommand::DetectBeats { clip: clip_id })) {
        Ok(ApplyOutcome::Edited(EditOutcome::Updated(_))) => {
            info!(%clip_id, "detected beats");
            publish_projection(engine, ui);
        }
        Ok(other) => error!(%clip_id, "unexpected detect-beats outcome: {other:?}"),
        Err(e) => error!(%clip_id, "detect beats failed: {e}"),
    }
}

/// Clear a clip's detected beat markers (M8 Phase 6). One undoable entry.
fn clear_beats_and_publish(engine: &mut Engine, clip: &str, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "clear-beats ignored: unparsable clip id");
        return;
    };
    match engine.apply(Command::Edit(EditCommand::ClearBeats { clip: clip_id })) {
        Ok(ApplyOutcome::Edited(EditOutcome::Updated(_))) => {
            info!(%clip_id, "cleared beats");
            publish_projection(engine, ui);
        }
        Ok(other) => error!(%clip_id, "unexpected clear-beats outcome: {other:?}"),
        Err(e) => error!(%clip_id, "clear beats failed: {e}"),
    }
}

/// Set the project canvas settings (M1): aspect preset + background color
/// in one undoable history entry. An out-of-range preset index falls back
/// to auto (defensive — the dialog's list is index-aligned with the model).
fn set_canvas_and_publish(
    engine: &mut Engine,
    aspect_index: i32,
    background: [u8; 3],
    ui: &UiSink,
) {
    let aspect = usize::try_from(aspect_index)
        .ok()
        .and_then(|i| cutlass_models::CanvasAspect::ALL.get(i).copied())
        .unwrap_or_default();
    match engine.apply(Command::Edit(EditCommand::SetCanvas { aspect, background })) {
        Ok(_) => {
            info!(aspect = aspect.name(), ?background, "set canvas settings");
            publish_projection(engine, ui);
        }
        Err(e) => error!("set canvas failed: {e}"),
    }
}

/// Set a visual clip's crop window + mirroring (CapCut crop, M1). One
/// undoable history entry; the engine validates the rect and rejects
/// audio-lane clips, so a failure here just logs (the inspector only shows
/// crop controls for visual clips — a rejection is a stale-projection race).
fn set_clip_crop_and_publish(
    engine: &mut Engine,
    clip: &str,
    crop: CropRect,
    flip_h: bool,
    flip_v: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-clip-crop ignored: unparsable clip id");
        return;
    };
    if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipCrop {
        clip: clip_id,
        crop,
        flip_h,
        flip_v,
    })) {
        error!(%clip_id, "set clip crop failed: {e}");
        return;
    }
    info!(
        %clip_id,
        x = crop.x, y = crop.y, w = crop.w, h = crop.h, flip_h, flip_v,
        "set clip crop"
    );
    publish_projection(engine, ui);
}

/// Set or clear a visual clip's filter preset. A live look drag may have left
/// an override in place; clear it first so the commit becomes authoritative.
fn set_clip_filter_and_publish(
    engine: &mut Engine,
    clip: &str,
    filter_id: &str,
    intensity: f32,
    ui: &UiSink,
) {
    engine.set_look_override(None);
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-clip-filter ignored: unparsable clip id");
        return;
    };
    let filter = filter_from_ui(filter_id, intensity);
    if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipFilter {
        clip: clip_id,
        filter: filter.clone(),
    })) {
        error!(%clip_id, filter_id, intensity, "set clip filter failed: {e}");
        return;
    }
    info!(%clip_id, ?filter, "set clip filter");
    publish_projection(engine, ui);
}

/// Set or clear a visual clip's `.cube` LUT (empty path clears). Intensity
/// blends the looked-up color over the original in the LUT pass itself.
fn set_clip_lut_and_publish(
    engine: &mut Engine,
    clip: &str,
    path: &str,
    intensity: f32,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-clip-lut ignored: unparsable clip id");
        return;
    };
    let lut = (!path.is_empty()).then(|| Lut {
        path: path.to_string(),
        intensity: intensity.clamp(0.0, 1.0),
    });
    if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipLut {
        clip: clip_id,
        lut: lut.clone(),
    })) {
        error!(%clip_id, path, intensity, "set clip LUT failed: {e}");
        return;
    }
    info!(%clip_id, ?lut, "set clip LUT");
    publish_projection(engine, ui);
}

/// Set all manual color adjustments on a visual clip in one undoable edit.
/// Release commits clear the live look override first, mirroring generator
/// and transform preview semantics.
fn set_clip_adjust_and_publish(
    engine: &mut Engine,
    clip: &str,
    adjust: ColorAdjustments,
    ui: &UiSink,
) {
    engine.set_look_override(None);
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-clip-adjust ignored: unparsable clip id");
        return;
    };
    let adjust = sanitize_adjustments(adjust);
    if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipAdjustments {
        clip: clip_id,
        adjust,
    })) {
        error!(%clip_id, ?adjust, "set clip adjustments failed: {e}");
        return;
    }
    info!(%clip_id, ?adjust, "set clip adjustments");
    publish_projection(engine, ui);
}

fn set_clip_animation_and_publish(
    engine: &mut Engine,
    clip: &str,
    slot: &str,
    animation_id: &str,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-clip-animation ignored: unparsable clip id");
        return;
    };
    let Some(animation_slot) = parse_animation_slot(slot) else {
        error!(slot, "set-clip-animation ignored: unknown slot");
        return;
    };
    let animation = if animation_id.is_empty() {
        None
    } else {
        Some(cutlass_models::AnimationRef::new(animation_id))
    };
    if let Err(e) = engine.apply(Command::Edit(EditCommand::SetClipAnimation {
        clip: clip_id,
        slot: animation_slot,
        animation,
    })) {
        error!(%clip_id, slot, animation_id, "set clip animation failed: {e}");
        return;
    }
    info!(%clip_id, slot, animation_id, "set clip animation");
    publish_projection(engine, ui);
}

fn parse_animation_slot(slot: &str) -> Option<cutlass_models::AnimationSlot> {
    match slot {
        "in" => Some(cutlass_models::AnimationSlot::In),
        "out" => Some(cutlass_models::AnimationSlot::Out),
        "combo" => Some(cutlass_models::AnimationSlot::Combo),
        _ => None,
    }
}

/// Append a catalog effect to a clip's chain (M4). One undoable entry; the
/// composite repaints because effects are visual.
fn add_effect_and_publish(engine: &mut Engine, clip: &str, effect_id: &str, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "add-effect ignored: unparsable clip id");
        return;
    };
    if let Err(e) = engine.apply(Command::Edit(EditCommand::AddEffect {
        clip: clip_id,
        effect_id: effect_id.to_string(),
    })) {
        error!(%clip_id, effect_id, "add effect failed: {e}");
        return;
    }
    info!(%clip_id, effect_id, "added effect");
    publish_projection(engine, ui);
}

/// Remove the effect at `index` from a clip's chain (M4).
fn remove_effect_and_publish(engine: &mut Engine, clip: &str, index: u32, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "remove-effect ignored: unparsable clip id");
        return;
    };
    if let Err(e) = engine.apply(Command::Edit(EditCommand::RemoveEffect {
        clip: clip_id,
        index: index as usize,
    })) {
        error!(%clip_id, index, "remove effect failed: {e}");
        return;
    }
    info!(%clip_id, index, "removed effect");
    publish_projection(engine, ui);
}

/// Set one effect parameter to a constant (M4). The inspector addresses the
/// parameter by its catalog name; resolve it to the uniform slot index the
/// command expects from the clip's current effect.
fn set_effect_param_and_publish(
    engine: &mut Engine,
    clip: &str,
    index: u32,
    param: &str,
    value: f32,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-effect-param ignored: unparsable clip id");
        return;
    };
    let slot = engine
        .project()
        .clip(clip_id)
        .and_then(|c| c.effects.get(index as usize))
        .and_then(|fx| cutlass_models::effect_spec(&fx.effect_id))
        .and_then(|spec| spec.params.iter().position(|p| p.name == param));
    let Some(slot) = slot else {
        error!(%clip_id, index, param, "set-effect-param ignored: unknown param");
        return;
    };
    if let Err(e) = engine.apply(Command::Edit(EditCommand::SetEffectParam {
        clip: clip_id,
        index: index as usize,
        param: slot,
        value,
    })) {
        error!(%clip_id, index, param, value, "set effect param failed: {e}");
        return;
    }
    info!(%clip_id, index, param, value, "set effect param");
    publish_projection(engine, ui);
}

/// Add a catalog transition at the junction after `clip` (M4). Requires a
/// right-neighbor clip that abuts; the engine rejects otherwise.
fn add_transition_and_publish(engine: &mut Engine, clip: &str, transition_id: &str, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "add-transition ignored: unparsable clip id");
        return;
    };
    if let Err(e) = engine.apply(Command::Edit(EditCommand::AddTransition {
        clip: clip_id,
        transition_id: transition_id.to_string(),
    })) {
        error!(%clip_id, transition_id, "add transition failed: {e}");
        return;
    }
    info!(%clip_id, transition_id, "added transition");
    publish_projection(engine, ui);
}

/// Remove the transition at `clip`'s right junction (M4).
fn remove_transition_and_publish(engine: &mut Engine, clip: &str, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "remove-transition ignored: unparsable clip id");
        return;
    };
    if let Err(e) = engine.apply(Command::Edit(EditCommand::RemoveTransition {
        clip: clip_id,
    })) {
        error!(%clip_id, "remove transition failed: {e}");
        return;
    }
    info!(%clip_id, "removed transition");
    publish_projection(engine, ui);
}

/// Set the window length (timeline ticks) of the transition after `clip` (M4).
fn set_transition_and_publish(engine: &mut Engine, clip: &str, duration: i64, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-transition ignored: unparsable clip id");
        return;
    };
    if let Err(e) = engine.apply(Command::Edit(EditCommand::SetTransition {
        clip: clip_id,
        duration,
    })) {
        error!(%clip_id, duration, "set transition failed: {e}");
        return;
    }
    info!(%clip_id, duration, "set transition duration");
    publish_projection(engine, ui);
}

/// Drop a ruler marker (M1). Empty `color` cycles the palette; one undoable
/// history entry.
fn add_marker_and_publish(
    engine: &mut Engine,
    at_tick: i64,
    name: &str,
    color: &str,
    tl_rate: Rational,
    ui: &UiSink,
) {
    let at = RationalTime::new(at_tick.max(0), tl_rate);
    let color = parse_marker_color(color);
    match engine.apply(Command::Edit(EditCommand::AddMarker {
        at,
        name: name.to_string(),
        color,
    })) {
        Ok(ApplyOutcome::Edited(EditOutcome::CreatedMarker(id))) => {
            info!(%id, at_tick, "added timeline marker");
            publish_projection(engine, ui);
        }
        Ok(other) => error!(at_tick, "unexpected add-marker outcome: {other:?}"),
        Err(e) => error!(at_tick, "add marker failed: {e}"),
    }
}

/// Remove a ruler marker by raw id (M1). One undoable history entry.
fn remove_marker_and_publish(engine: &mut Engine, marker: &str, ui: &UiSink) {
    let Some(marker_id) = parse_raw_id(marker).map(MarkerId::from_raw) else {
        error!(marker, "remove-marker ignored: unparsable marker id");
        return;
    };
    match engine.apply(Command::Edit(EditCommand::RemoveMarker {
        marker: marker_id,
    })) {
        Ok(_) => {
            info!(%marker_id, "removed timeline marker");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%marker_id, "remove marker failed: {e}"),
    }
}

/// Move / rename / recolor a ruler marker (M1). One undoable history entry.
fn set_marker_and_publish(
    engine: &mut Engine,
    marker: &str,
    at_tick: i64,
    name: &str,
    color: &str,
    tl_rate: Rational,
    ui: &UiSink,
) {
    let Some(marker_id) = parse_raw_id(marker).map(MarkerId::from_raw) else {
        error!(marker, "set-marker ignored: unparsable marker id");
        return;
    };
    let at = RationalTime::new(at_tick.max(0), tl_rate);
    let color = parse_marker_color(color)
        .or_else(|| {
            engine
                .project()
                .timeline()
                .marker(marker_id)
                .map(|m| m.color)
        })
        .unwrap_or(MarkerColor::Teal);
    match engine.apply(Command::Edit(EditCommand::SetMarker {
        marker: marker_id,
        at,
        name: name.to_string(),
        color,
    })) {
        Ok(_) => {
            info!(%marker_id, at_tick, "updated timeline marker");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%marker_id, "set marker failed: {e}"),
    }
}

fn remove_track_manual_and_publish(engine: &mut Engine, track: &str, ui: &UiSink) {
    let Some(track_id) = parse_raw_id(track).map(TrackId::from_raw) else {
        error!(track, "remove-track ignored: unparsable track id");
        return;
    };
    // CapCut never deletes the main track (the UI hides the menu item; this
    // guards races where the projection lagged a main-lane promotion).
    if Some(track_id) == main_video_track(engine) {
        info!(%track_id, "remove-track ignored: main track is permanent");
        return;
    }
    match engine.apply(Command::Edit(EditCommand::RemoveTrack { track: track_id })) {
        Ok(_) => {
            info!(%track_id, "removed track");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%track_id, "remove track failed: {e}"),
    }
}

fn move_track_manual_and_publish(engine: &mut Engine, track: &str, index: usize, ui: &UiSink) {
    let Some(track_id) = parse_raw_id(track).map(TrackId::from_raw) else {
        error!(track, "move-track ignored: unparsable track id");
        return;
    };
    match engine.apply(Command::Edit(EditCommand::MoveTrack {
        track: track_id,
        index,
    })) {
        Ok(_) => {
            info!(%track_id, index, "moved track");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%track_id, index, "move track failed: {e}"),
    }
}

fn set_track_name_and_publish(engine: &mut Engine, track: &str, name: &str, ui: &UiSink) {
    let Some(track_id) = parse_raw_id(track).map(TrackId::from_raw) else {
        error!(track, "set-track-name ignored: unparsable track id");
        return;
    };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return;
    }
    match engine.apply(Command::Edit(EditCommand::SetTrackName {
        track: track_id,
        name: trimmed.to_string(),
    })) {
        Ok(_) => {
            info!(%track_id, name = trimmed, "renamed track");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%track_id, "rename track failed: {e}"),
    }
}

fn parse_marker_color(name: &str) -> Option<MarkerColor> {
    match name {
        "teal" => Some(MarkerColor::Teal),
        "blue" => Some(MarkerColor::Blue),
        "purple" => Some(MarkerColor::Purple),
        "pink" => Some(MarkerColor::Pink),
        "red" => Some(MarkerColor::Red),
        "orange" => Some(MarkerColor::Orange),
        "yellow" => Some(MarkerColor::Yellow),
        "green" => Some(MarkerColor::Green),
        _ => None,
    }
}

/// Build a shape generator with new reference-pixel dimensions, preserving the
/// clip's shape kind and fill. `None` when the clip is missing or not a shape.
///
/// Dimensions are floored at 1px and non-finite input is rejected: the slider
/// stays in `8..=1920`, but a typed entry or double-click reset can deliver
/// anything, and a zero/negative extent would collapse the raster's `Rect` to
/// an invisible shape.
fn shape_size_from_engine(
    engine: &Engine,
    clip: &str,
    width: f32,
    height: f32,
) -> Option<Generator> {
    if !width.is_finite() || !height.is_finite() {
        return None;
    }
    let clip_id = parse_raw_id(clip).map(ClipId::from_raw)?;
    let generator = match &engine.project().timeline().clip(clip_id)?.content {
        ClipSource::Generated(g) => g,
        ClipSource::Media { .. } => return None,
    };
    match generator {
        // A slider commit sets an absolute size, so the animated params
        // collapse to constants (matching the pre-keyframe behavior); corner
        // rounding and stroke ride along untouched.
        Generator::Shape {
            shape,
            rgba,
            corner_radius,
            stroke,
            ..
        } => Some(Generator::Shape {
            shape: shape.clone(),
            rgba: rgba.clone(),
            width: Param::Constant(width.max(1.0)),
            height: Param::Constant(height.max(1.0)),
            corner_radius: corner_radius.clone(),
            stroke: stroke.clone(),
        }),
        _ => None,
    }
}

/// Replace a generated clip's content (inspector title edit). One history
/// entry per committed edit; the engine rejects non-generated clips.
fn set_generator_and_publish(engine: &mut Engine, clip: &str, generator: Generator, ui: &UiSink) {
    // A live font-size drag may have left an override in place; the commit is
    // the authoritative value, so clear it (the next render is identical — no
    // flicker between drag end and commit, mirroring `SetTransform`).
    engine.set_generator_override(None);
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "set-generator ignored: unparsable clip id");
        return;
    };
    match engine.apply(Command::Edit(EditCommand::SetGenerator {
        clip: clip_id,
        generator,
    })) {
        Ok(_) => {
            info!(%clip_id, "updated generated clip content");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%clip_id, "set generator failed: {e}"),
    }
}

/// Whether `[start, start + duration)` overlaps no clip on `track`.
fn span_free(track: &Track, start: i64, duration: i64) -> bool {
    let end = start + duration;
    track
        .clips_ordered()
        .iter()
        .all(|c| c.timeline.end_tick() <= start || c.timeline.start.value >= end)
}

/// `track` (raw id from the Slint projection) when it names an existing lane
/// of `kind`.
fn lane_of_kind(engine: &Engine, track: &str, kind: TrackKind) -> Option<TrackId> {
    let id = TrackId::from_raw(parse_raw_id(track)?);
    engine
        .project()
        .timeline()
        .track(id)
        .is_some_and(|t| t.kind == kind)
        .then_some(id)
}

/// The main lane while it's still empty — CapCut lands the first video
/// dropped *anywhere* on the main track rather than spawning an overlay lane.
fn empty_main_lane(engine: &Engine, kind: TrackKind) -> Option<TrackId> {
    if kind != TrackKind::Video {
        return None;
    }
    let timeline = engine.project().timeline();
    let main = timeline.main_track()?;
    timeline
        .track(main)
        .is_some_and(cutlass_models::Track::is_empty)
        .then_some(main)
}

/// Move a dragged clip to its resolved landing spot: an existing lane
/// (`track` set) or a new lane of the clip's kind inserted at `insert_row`.
/// A cross-lane move that empties its source lane removes that lane
/// (CapCut deletes overlay tracks that empty out). With `insert` (main-track
/// magnet) the landing is an insertion on the main lane; with the magnet on,
/// a move *off* the main lane also closes the gap it leaves. Every variant
/// is one history group, so one undo reverts the whole gesture.
#[allow(clippy::too_many_arguments)]
fn move_clip_and_publish(
    engine: &mut Engine,
    clip: &str,
    track: &str,
    insert_row: i64,
    start_tick: i64,
    insert: bool,
    main_magnet: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "move ignored: unparsable clip id");
        return;
    };
    let Some(source_track) = engine.project().timeline().track_of(clip_id) else {
        error!(%clip_id, "move ignored: clip not on the timeline");
        return;
    };
    let kind = engine
        .project()
        .timeline()
        .track(source_track)
        .expect("track_of returned an existing track")
        .kind;
    let placed = engine
        .project()
        .clip(clip_id)
        .expect("track_of returned a placed clip")
        .timeline;
    let tl_rate = engine.project().timeline().frame_rate;
    // Decided before the gesture mutates anything: a new lane created below
    // the stack would become the bottom video lane and steal main status.
    let source_is_main = main_magnet && Some(source_track) == main_video_track(engine);

    if insert {
        // Main-track magnet: the resolver targets the existing main lane.
        let Some(to_track) = parse_raw_id(track).map(TrackId::from_raw) else {
            error!(%clip_id, track, "insert-move ignored: unparsable track id");
            return;
        };
        engine.begin_group();
        let result = if to_track == source_track {
            ripple_reorder(engine, clip_id, to_track, start_tick.max(0))
        } else {
            ripple_move_in(engine, clip_id, source_track, to_track, start_tick.max(0))
        };
        match result {
            Ok(()) => {
                engine.commit_group();
                info!(%clip_id, %to_track, start_tick, "ripple-inserted moved clip");
            }
            Err(e) => {
                error!(%clip_id, %to_track, start_tick, "insert move failed: {e}");
                engine.rollback_group();
            }
        }
        publish_projection(engine, ui);
        return;
    }

    // One history entry per move, including a created destination lane and a
    // removed emptied source lane.
    engine.begin_group();
    let to_track = match parse_raw_id(track).map(TrackId::from_raw) {
        Some(id) => id,
        None => match create_track(engine, kind, insert_row) {
            Ok(id) => id,
            Err(e) => {
                error!(%clip_id, "move failed creating {kind:?} track: {e}");
                engine.rollback_group();
                return;
            }
        },
    };

    match engine.apply(Command::Edit(EditCommand::MoveClip {
        clip: clip_id,
        to_track,
        start: RationalTime::new(start_tick.max(0), tl_rate),
    })) {
        Ok(ApplyOutcome::Edited(EditOutcome::Updated(_))) => {
            let mut completed = true;
            if source_track != to_track {
                // Leaving the main lane with the magnet on closes the gap
                // the clip vacated (CapCut ripple). Can't collide: the first
                // shifted clip lands exactly where the moved clip started.
                if source_is_main {
                    completed = apply_edit(
                        engine,
                        EditCommand::ShiftClips {
                            track: source_track,
                            from: placed.start,
                            delta: RationalTime::new(-placed.duration.value, tl_rate),
                        },
                    )
                    .map_err(|e| error!(%clip_id, "move failed closing main-lane gap: {e}"))
                    .is_ok();
                }
                if completed {
                    remove_track_if_empty(engine, source_track);
                }
            }
            if completed {
                engine.commit_group();
                info!(%clip_id, %to_track, start_tick, "moved clip");
            } else {
                engine.rollback_group();
            }
            publish_projection(engine, ui);
        }
        Ok(other) => {
            error!(%clip_id, "unexpected move-clip outcome: {other:?}");
            engine.rollback_group();
            publish_projection(engine, ui);
        }
        // The drag resolver previewed a valid spot; the engine still rejects
        // atomically if the projection raced a concurrent edit. Rolling back
        // removes a lane this move just created.
        Err(e) => {
            error!(%clip_id, %to_track, start_tick, "move clip failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
        }
    }
}

/// Reorder within the main lane as one group of four commands: park the clip
/// past the lane's content end (never rendered — the projection publishes
/// only after the group resolves), close its old gap, open the new hole at
/// `at` (post-close space, straight from the drag resolver), and land in it.
fn ripple_reorder(
    engine: &mut Engine,
    clip_id: ClipId,
    track: TrackId,
    at: i64,
) -> Result<(), String> {
    let tl_rate = engine.project().timeline().frame_rate;
    let placed = engine
        .project()
        .clip(clip_id)
        .ok_or("clip not on the timeline")?
        .timeline;
    let duration = placed.duration.value;
    let park = engine
        .project()
        .timeline()
        .track(track)
        .ok_or("main lane missing")?
        .content_end();

    apply_edit(
        engine,
        EditCommand::MoveClip {
            clip: clip_id,
            to_track: track,
            start: RationalTime::new(park, tl_rate),
        },
    )?;
    // Both shifts also carry the parked clip along (its start stays past the
    // rest of the lane), so it never collides with the clips in between.
    apply_edit(
        engine,
        EditCommand::ShiftClips {
            track,
            from: placed.start,
            delta: RationalTime::new(-duration, tl_rate),
        },
    )?;
    apply_edit(
        engine,
        EditCommand::ShiftClips {
            track,
            from: RationalTime::new(at, tl_rate),
            delta: RationalTime::new(duration, tl_rate),
        },
    )?;
    apply_edit(
        engine,
        EditCommand::MoveClip {
            clip: clip_id,
            to_track: track,
            start: RationalTime::new(at, tl_rate),
        },
    )
}

/// Cross-lane move onto the main lane: open the hole at `at`, move the clip
/// in, and drop the source lane when this emptied it (same overlay policy as
/// freeform moves).
fn ripple_move_in(
    engine: &mut Engine,
    clip_id: ClipId,
    source_track: TrackId,
    to_track: TrackId,
    at: i64,
) -> Result<(), String> {
    let tl_rate = engine.project().timeline().frame_rate;
    let duration = engine
        .project()
        .clip(clip_id)
        .ok_or("clip not on the timeline")?
        .timeline
        .duration
        .value;

    apply_edit(
        engine,
        EditCommand::ShiftClips {
            track: to_track,
            from: RationalTime::new(at, tl_rate),
            delta: RationalTime::new(duration, tl_rate),
        },
    )?;
    apply_edit(
        engine,
        EditCommand::MoveClip {
            clip: clip_id,
            to_track,
            start: RationalTime::new(at, tl_rate),
        },
    )?;
    remove_track_if_empty(engine, source_track);
    Ok(())
}

/// Re-place a trimmed clip at its resolved extent. The trim resolver already
/// clamped to neighbors and source headroom, so this should always apply; the
/// engine still validates atomically (overlap, source bounds) and we surface
/// any rejection rather than mutating the projection optimistically.
///
/// With `linkage` on, the same edge delta applies to every clip in the
/// trimmed clip's link group (the resolver intersected the clamps, so the
/// partners' extents are valid too) — one history entry for the group.
///
/// With the main-track magnet on and the trim touching the main lane, the
/// trim *ripples* instead of leaving/eating a gap: downstream clips follow
/// the dragged edge (timeline roadmap Phase 7's deliberate gap). See
/// [`commit_trims`]; still one history entry — a single undo restores the
/// trim and every shifted clip.
fn trim_clip_and_publish(
    engine: &mut Engine,
    clip: &str,
    start_tick: i64,
    duration_ticks: i64,
    linkage: bool,
    main_magnet: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "trim ignored: unparsable clip id");
        return;
    };
    let Some(placed) = engine.project().clip(clip_id).map(|c| c.timeline) else {
        error!(%clip_id, "trim ignored: clip not on the timeline");
        return;
    };
    let tl_rate = engine.project().timeline().frame_rate;
    let start = start_tick.max(0);
    let duration = duration_ticks.max(1);
    // The same edge motion, expressed as deltas the partners can replay.
    let delta_start = start - placed.start.value;
    let delta_duration = duration - placed.duration.value;

    let mut trims = vec![(clip_id, TimeRange::at_rate(start, duration, tl_rate))];
    if linkage {
        for partner in link_group_ids(engine, clip_id) {
            if partner == clip_id {
                continue;
            }
            let Some(extent) = engine.project().clip(partner).map(|c| c.timeline) else {
                continue;
            };
            trims.push((
                partner,
                TimeRange::at_rate(
                    extent.start.value + delta_start,
                    (extent.duration.value + delta_duration).max(1),
                    tl_rate,
                ),
            ));
        }
    }

    match commit_trims(engine, &trims, main_magnet) {
        Ok(ripple) => info!(%clip_id, start_tick, duration_ticks, linkage, ripple, "trimmed clip"),
        Err(e) => error!(%clip_id, "trim clip failed: {e}"),
    }
    publish_projection(engine, ui);
}

/// Apply a resolved set of member trims as one history group.
///
/// With the main-track magnet on and any member sitting on the main lane
/// (dragging the audio half of a linked pair must still keep the main lane
/// gapless), every member's trim ripples on its own lane — linked pairs and
/// their downstream neighbors all shift by the same duration delta, so
/// cross-lane alignment survives. Otherwise members get plain `TrimClip`s.
///
/// A rejected step rolls the whole group back — no half-applied ripple.
/// Returns whether the group rippled.
fn commit_trims(
    engine: &mut Engine,
    trims: &[(ClipId, TimeRange)],
    main_magnet: bool,
) -> Result<bool, String> {
    let main = main_video_track(engine);
    let ripple = main_magnet
        && main.is_some_and(|m| {
            trims
                .iter()
                .any(|&(id, _)| engine.project().timeline().track_of(id) == Some(m))
        });

    engine.begin_group();
    for &(id, timeline) in trims {
        let result = if ripple {
            apply_ripple_trim(engine, id, timeline)
        } else {
            apply_edit(engine, EditCommand::TrimClip { clip: id, timeline })
        };
        if let Err(e) = result {
            engine.rollback_group();
            return Err(format!("clip {id}: {e}"));
        }
    }
    engine.commit_group();
    Ok(ripple)
}

/// One member's ripple trim: `TrimClip` + `ShiftClips` composed on the
/// member's own lane, ordered so the engine's atomic validation accepts the
/// intermediate state (open room before growing into it, trim before
/// closing the gap behind a shrink).
///
/// Semantics (CapCut): the trimmed clip stays anchored at its old start and
/// every downstream clip shifts by the duration delta — the lane neither
/// leaves nor eats a gap.
/// - Trailing edge: only the duration changes; downstream (clips starting at
///   or after the old end) shifts by the delta.
/// - Leading edge: the resolved extent moves the start — that start delta is
///   what the engine derives the new source in-point from — and the shift
///   then re-anchors the clip at its old start, carrying downstream along.
///   A leading grow shifts everything from the old start right first, then
///   trims anchored there, which yields the same negative start delta.
///
/// The caller wraps members in one history group and rolls back on error,
/// so a rejected step never leaves a half-applied ripple.
fn apply_ripple_trim(engine: &mut Engine, clip: ClipId, timeline: TimeRange) -> Result<(), String> {
    let Some(old) = engine.project().clip(clip).map(|c| c.timeline) else {
        return Err("clip is not on the timeline".into());
    };
    let Some(track) = engine.project().timeline().track_of(clip) else {
        return Err("clip has no track".into());
    };
    let tl_rate = engine.project().timeline().frame_rate;
    let delta_dur = timeline.duration.value - old.duration.value;
    let trim = EditCommand::TrimClip { clip, timeline };

    if timeline.start.value != old.start.value {
        // Leading edge (the resolver anchors the end, so the start moved).
        if delta_dur > 0 {
            // Grow: open room first (the clip and everything after it move
            // right), then trim anchored at the old start.
            apply_edit(
                engine,
                EditCommand::ShiftClips {
                    track,
                    from: old.start,
                    delta: RationalTime::new(delta_dur, tl_rate),
                },
            )?;
            apply_edit(
                engine,
                EditCommand::TrimClip {
                    clip,
                    timeline: TimeRange::at_rate(old.start.value, timeline.duration.value, tl_rate),
                },
            )
        } else {
            // Shrink: trim to the resolved extent (a gap opens at the old
            // start), then slide the clip and downstream left into it.
            apply_edit(engine, trim)?;
            apply_edit(
                engine,
                EditCommand::ShiftClips {
                    track,
                    from: timeline.start,
                    delta: RationalTime::new(old.start.value - timeline.start.value, tl_rate),
                },
            )
        }
    } else if delta_dur > 0 {
        // Trailing grow: push downstream right, then extend into the hole.
        apply_edit(
            engine,
            EditCommand::ShiftClips {
                track,
                from: RationalTime::new(old.end_tick(), tl_rate),
                delta: RationalTime::new(delta_dur, tl_rate),
            },
        )?;
        apply_edit(engine, trim)
    } else if delta_dur < 0 {
        // Trailing shrink: pull the edge in, then close the gap behind it.
        apply_edit(engine, trim)?;
        apply_edit(
            engine,
            EditCommand::ShiftClips {
                track,
                from: RationalTime::new(old.end_tick(), tl_rate),
                delta: RationalTime::new(delta_dur, tl_rate),
            },
        )
    } else {
        // No edge moved (defensive — the UI skips noop trims).
        apply_edit(engine, trim)
    }
}

/// Every clip sharing `clip`'s link group (including itself); just the clip
/// when it's unlinked. O(total clips) — cold per-gesture path.
fn link_group_ids(engine: &Engine, clip: ClipId) -> Vec<ClipId> {
    let Some(link) = engine.project().clip(clip).and_then(|c| c.link) else {
        return vec![clip];
    };
    engine
        .project()
        .timeline()
        .tracks_ordered()
        .flat_map(|t| t.clips_ordered())
        .filter(|c| c.link == Some(link))
        .map(|c| c.id)
        .collect()
}

/// Toggle a track header flag (hide/mute/lock). Undoable like any edit; the
/// republished projection carries the new flag to the lane header. Disabling
/// a visual track drops it from the composite (the engine skips `!enabled`
/// visual tracks), so the preview catches up on the next scrub.
fn set_track_flag_and_publish(
    engine: &mut Engine,
    track: &str,
    flag: TrackFlag,
    value: bool,
    ui: &UiSink,
) {
    let Some(track_id) = parse_raw_id(track).map(TrackId::from_raw) else {
        error!(track, "set-track-flag ignored: unparsable track id");
        return;
    };
    let command = match flag {
        TrackFlag::Enabled => EditCommand::SetTrackEnabled {
            track: track_id,
            enabled: value,
        },
        TrackFlag::Muted => EditCommand::SetTrackMuted {
            track: track_id,
            muted: value,
        },
        TrackFlag::Locked => EditCommand::SetTrackLocked {
            track: track_id,
            locked: value,
        },
        TrackFlag::DuckSource => EditCommand::SetTrackDuckSource {
            track: track_id,
            duck_source: value,
        },
    };
    match engine.apply(Command::Edit(command)) {
        Ok(ApplyOutcome::Edited(EditOutcome::UpdatedTrack(_))) => {
            info!(%track_id, value, "set track flag");
            publish_projection(engine, ui);
        }
        Ok(other) => error!(%track_id, "unexpected set-track-flag outcome: {other:?}"),
        Err(e) => error!(%track_id, "set track flag failed: {e}"),
    }
}

/// Shared flags between the worker loop and the export thread. `active`
/// gates one-job-at-a-time (the export thread clears it when it exits);
/// `cancel` is reset at job start and set by [`WorkerMsg::CancelExport`].
#[derive(Default)]
struct ExportJobState {
    active: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

/// One snapshot of the export job for the Slint `ExportBackend` global.
#[derive(Default)]
struct ExportUiState {
    running: bool,
    done: u64,
    total: u64,
    completed: bool,
    failed: bool,
    status: String,
}

fn publish_export_state(weak: &slint::Weak<ExportBackend<'static>>, state: ExportUiState) {
    let weak = weak.clone();
    if let Err(e) = slint::invoke_from_event_loop(move || {
        if let Some(backend) = weak.upgrade() {
            backend.set_running(state.running);
            backend.set_frames_done(state.done.min(i32::MAX as u64) as i32);
            backend.set_frames_total(state.total.min(i32::MAX as u64) as i32);
            backend.set_progress(if state.total > 0 {
                (state.done as f32 / state.total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            });
            backend.set_completed(state.completed);
            backend.set_failed(state.failed);
            backend.set_status(state.status.into());
        }
    }) {
        error!("failed to publish export state to UI: {e}");
    }
}

/// The output settings for one export request: the dialog's height preset
/// scales the composite canvas (aspect preserved), the fps preset overrides
/// the sampling rate, and everything is evened for H.264.
fn export_settings_for(
    project: &cutlass_models::Project,
    request: &ExportRequest,
) -> ExportSettings {
    let mut settings = ExportSettings::for_project(project);
    if let Some(target_h) = request.target_height.filter(|&h| h > 0) {
        let (w, h) = settings.size;
        if h > 0 {
            let scaled_w =
                ((u64::from(w) * u64::from(target_h) + u64::from(h) / 2) / u64::from(h)) as u32;
            settings.size = (scaled_w.max(2), target_h);
        }
    }
    if let Some(num) = request.fps_num.filter(|&n| n > 0) {
        settings.frame_rate = Rational::new(num, 1);
    }
    settings.evened()
}

/// Snapshot the project and run the export on a dedicated thread: decode +
/// GPU composite + encode would otherwise freeze preview and edits for the
/// whole render. The thread brings up its own headless [`Renderer`] (own GPU
/// queue + decoder cache — the mobile `export_job.rs` pattern), publishes
/// progress to the UI at most ~10×/sec, and tears the `active` gate down
/// when it exits — whatever the outcome.
fn start_export(engine: &Engine, ui: &UiSink, state: &ExportJobState, request: ExportRequest) {
    if state.active.swap(true, Ordering::SeqCst) {
        warn!("export refused: a job is already running");
        return;
    }
    state.cancel.store(false, Ordering::SeqCst);

    let project = engine.project().clone();
    let settings = export_settings_for(&project, &request);
    let export_weak = ui.export.clone();
    let active = state.active.clone();
    let cancel = state.cancel.clone();
    let path = request.path;

    publish_export_state(
        &export_weak,
        ExportUiState {
            running: true,
            ..Default::default()
        },
    );

    let spawned = std::thread::Builder::new()
        .name("cutlass-export".into())
        .spawn(move || {
            info!(path = %path.display(), size = ?settings.size, "export job started");
            let weak = export_weak.clone();
            let mut last_publish = Instant::now();
            let mut published_once = false;
            let result = Renderer::new_headless().and_then(|mut renderer| {
                cutlass_render::export_to_file_observed(
                    &mut renderer,
                    &project,
                    &path,
                    settings,
                    &mut |done, total| {
                        if cancel.load(Ordering::Relaxed) {
                            return false;
                        }
                        // Throttle event-loop traffic, but always deliver the
                        // first call (the dialog learns the total) and the last.
                        if !published_once
                            || done == total
                            || last_publish.elapsed() >= Duration::from_millis(100)
                        {
                            published_once = true;
                            last_publish = Instant::now();
                            publish_export_state(
                                &weak,
                                ExportUiState {
                                    running: true,
                                    done,
                                    total,
                                    ..Default::default()
                                },
                            );
                        }
                        true
                    },
                )
            });

            let outcome = match result {
                Ok(frames) => {
                    info!(
                        frames,
                        width = settings.size.0,
                        height = settings.size.1,
                        path = %path.display(),
                        "export job finished"
                    );
                    ExportUiState {
                        done: frames,
                        total: frames,
                        completed: true,
                        status: format!(
                            "Saved {}×{}, {} frames to {}",
                            settings.size.0,
                            settings.size.1,
                            frames,
                            path.display()
                        ),
                        ..Default::default()
                    }
                }
                Err(RenderError::Cancelled) => {
                    info!(path = %path.display(), "export job cancelled");
                    ExportUiState {
                        failed: true,
                        status: "Export cancelled".into(),
                        ..Default::default()
                    }
                }
                Err(e) => {
                    error!(path = %path.display(), "export job failed: {e}");
                    ExportUiState {
                        failed: true,
                        status: format!("Export failed: {e}"),
                        ..Default::default()
                    }
                }
            };
            publish_export_state(&weak, outcome);
            active.store(false, Ordering::SeqCst);
        });

    if let Err(e) = spawned {
        error!("failed to spawn export thread: {e}");
        state.active.store(false, Ordering::SeqCst);
        publish_export_state(
            &ui.export,
            ExportUiState {
                failed: true,
                status: format!("Export failed to start: {e}"),
                ..Default::default()
            },
        );
    }
}

/// Remove every clip in `clips`; lanes the removals empty are removed with
/// them (CapCut deletes emptied overlay tracks — same policy the drag-moves
/// use). With the main-track magnet on, main-lane deletions ripple their
/// gaps closed. Everything forms one history group: one undo restores the
/// whole selection.
fn remove_clips_and_publish(engine: &mut Engine, clips: &[String], main_magnet: bool, ui: &UiSink) {
    let main = main_video_track(engine);
    // Resolve every member up front: a single bad id voids the whole batch
    // rather than half-deleting the selection.
    let mut targets = Vec::with_capacity(clips.len());
    for clip in clips {
        let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
            error!(clip, "delete ignored: unparsable clip id");
            return;
        };
        let Some(track) = engine.project().timeline().track_of(clip_id) else {
            error!(%clip_id, "delete ignored: clip not on the timeline");
            return;
        };
        targets.push((clip_id, track));
    }
    if targets.is_empty() {
        return;
    }
    // Ripple deletes shift later main-lane clips left; deleting right-to-left
    // keeps each pending member's recorded position valid.
    targets.sort_by_key(|(clip_id, _)| {
        std::cmp::Reverse(
            engine
                .project()
                .clip(*clip_id)
                .map(|c| c.timeline.start.value)
                .unwrap_or(0),
        )
    });

    engine.begin_group();
    for &(clip_id, track) in &targets {
        let command = if main_magnet && Some(track) == main {
            EditCommand::RippleDelete { clip: clip_id }
        } else {
            EditCommand::RemoveClip { clip: clip_id }
        };
        if let Err(e) = apply_edit(engine, command) {
            error!(%clip_id, "remove clip failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    // Lane cleanup after all removals: dedupe so each lane is checked once.
    let mut lanes: Vec<TrackId> = targets.iter().map(|&(_, track)| track).collect();
    lanes.sort();
    lanes.dedup();
    for lane in lanes {
        remove_track_if_empty(engine, lane);
    }
    engine.commit_group();
    info!(count = targets.len(), "removed clips");
    publish_projection(engine, ui);
}

/// Delete every clip in `clips` and close each lane's gap, always via
/// `RippleDelete` — the explicit ripple-delete gesture, independent of the
/// main-track magnet. One history group.
fn ripple_delete_clips_and_publish(engine: &mut Engine, clips: &[String], ui: &UiSink) {
    let mut targets = Vec::with_capacity(clips.len());
    for clip in clips {
        let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
            error!(clip, "ripple delete ignored: unparsable clip id");
            return;
        };
        let Some(track) = engine.project().timeline().track_of(clip_id) else {
            error!(%clip_id, "ripple delete ignored: clip not on the timeline");
            return;
        };
        targets.push((clip_id, track));
    }
    if targets.is_empty() {
        return;
    }
    targets.sort_by_key(|(clip_id, _)| {
        std::cmp::Reverse(
            engine
                .project()
                .clip(*clip_id)
                .map(|c| c.timeline.start.value)
                .unwrap_or(0),
        )
    });

    engine.begin_group();
    for &(clip_id, _) in &targets {
        if let Err(e) = apply_edit(engine, EditCommand::RippleDelete { clip: clip_id }) {
            error!(%clip_id, "ripple delete failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    let mut lanes: Vec<TrackId> = targets.iter().map(|&(_, track)| track).collect();
    lanes.sort();
    lanes.dedup();
    for lane in lanes {
        remove_track_if_empty(engine, lane);
    }
    engine.commit_group();
    info!(count = targets.len(), "ripple-deleted clips");
    publish_projection(engine, ui);
}

/// Toggle reverse playback on a media clip: keep the current speed and flip
/// `reversed`. With linkage on the whole link group follows in one history
/// entry.
fn reverse_clip_and_publish(engine: &mut Engine, clip: &str, linkage: bool, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "reverse ignored: unparsable clip id");
        return;
    };
    let Some(model) = engine.project().clip(clip_id).cloned() else {
        error!(%clip_id, "reverse ignored: clip not on the timeline");
        return;
    };
    if model.source_range().is_none() {
        error!(%clip_id, "reverse ignored: generated clip");
        return;
    }
    set_clip_speed_and_publish(
        engine,
        clip,
        model.speed.num,
        model.speed.den,
        !model.reversed,
        linkage,
        ui,
    );
}

/// CapCut "extract audio": place a linked audio-lane companion that reuses the
/// video clip's media (no new library asset). The video half goes silent via
/// [`Timeline::carries_own_audio`] once linked to the audio partner.
fn extract_audio_and_publish(engine: &mut Engine, clip: &str, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "extract audio ignored: unparsable clip id");
        return;
    };
    match extract_audio(engine, clip_id) {
        Ok(audio_clip) => {
            info!(%clip_id, %audio_clip, "extracted audio onto audio lane");
            publish_projection(engine, ui);
        }
        Err(e) => {
            // Core extraction is strict and atomic; a repeated or ineligible
            // UI gesture is therefore safe to surface as soft feedback.
            info!(%clip_id, "extract audio ignored: {e}");
        }
    }
}

/// Delegate the entire gesture to one atomic engine command.
fn extract_audio(engine: &mut Engine, clip_id: ClipId) -> Result<ClipId, String> {
    let audio_clip = match engine.apply(Command::Edit(EditCommand::ExtractAudio {
        clip: clip_id,
        to_track: None,
    })) {
        Ok(ApplyOutcome::Edited(EditOutcome::Created(id))) => id,
        Ok(other) => {
            return Err(format!("unexpected extract-audio outcome: {other:?}"));
        }
        Err(e) => return Err(format!("ignored: {e}")),
    };
    Ok(audio_clip)
}

/// Split a clip into two abutting clips at `at_tick`. The UI only offers the
/// split while the playhead is strictly inside the clip; the engine still
/// validates the position atomically.
///
/// With `linkage` on, every linked partner that also spans `at_tick` splits
/// at the same tick, and the resulting tails are linked into a fresh group
/// (heads keep the original link) — one history entry for the lot.
fn split_clip_and_publish(
    engine: &mut Engine,
    clip: &str,
    at_tick: i64,
    linkage: bool,
    ui: &UiSink,
) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "split ignored: unparsable clip id");
        return;
    };
    let tl_rate = engine.project().timeline().frame_rate;
    let at = RationalTime::new(at_tick, tl_rate);

    // Partners split only where the tick is strictly inside their extent
    // (linked clips can have different lengths after asymmetric edits).
    let members: Vec<ClipId> = if linkage {
        link_group_ids(engine, clip_id)
            .into_iter()
            .filter(|&id| {
                engine.project().clip(id).is_some_and(|c| {
                    at_tick > c.timeline.start.value && at_tick < c.timeline.end_tick()
                })
            })
            .collect()
    } else {
        vec![clip_id]
    };
    if members.is_empty() {
        error!(%clip_id, at_tick, "split ignored: tick outside the clip");
        return;
    }

    engine.begin_group();
    let mut tails = Vec::with_capacity(members.len());
    for member in &members {
        match engine.apply(Command::Edit(EditCommand::SplitClip { clip: *member, at })) {
            Ok(ApplyOutcome::Edited(EditOutcome::Created(tail))) => tails.push(tail),
            Ok(other) => {
                error!(%member, "unexpected split-clip outcome: {other:?}");
                engine.rollback_group();
                return;
            }
            Err(e) => {
                error!(%member, at_tick, "split clip failed: {e}");
                engine.rollback_group();
                return;
            }
        }
    }
    // Tails are born unlinked (split copies content, not links); pair them
    // back up so each half keeps moving as a unit.
    if tails.len() > 1
        && let Err(e) = apply_edit(
            engine,
            EditCommand::LinkClips {
                clips: tails.clone(),
            },
        )
    {
        error!(%clip_id, "split failed linking tails: {e}");
        engine.rollback_group();
        return;
    }
    engine.commit_group();
    info!(%clip_id, ?tails, at_tick, "split clip");
    publish_projection(engine, ui);
}

/// Land a group-drag batch. The resolver already validated every member
/// against everything outside the selection, but members can still collide
/// with *each other's* old positions mid-batch, so the batch goes
/// park-then-place: every member first parks past the global content end on
/// its target lane, then lands on its resolved start. One history group —
/// one undo reverts the whole gesture. Source lanes the moves empty are
/// removed (same overlay policy as single moves). Group moves are freeform —
/// the main-track magnet's ripple-insert applies to single-clip drags only.
fn move_group_and_publish(engine: &mut Engine, moves: &[GroupMove], ui: &UiSink) {
    // Resolve raw ids up front; any stale entry voids the batch.
    let mut resolved = Vec::with_capacity(moves.len());
    for entry in moves {
        let Some(clip_id) = parse_raw_id(&entry.clip).map(ClipId::from_raw) else {
            error!(clip = entry.clip, "group move ignored: unparsable clip id");
            return;
        };
        let Some(to_track) = parse_raw_id(&entry.track).map(TrackId::from_raw) else {
            error!(
                track = entry.track,
                "group move ignored: unparsable track id"
            );
            return;
        };
        let Some(source_track) = engine.project().timeline().track_of(clip_id) else {
            error!(%clip_id, "group move ignored: clip not on the timeline");
            return;
        };
        resolved.push((clip_id, to_track, source_track, entry.start_tick.max(0)));
    }
    if resolved.is_empty() {
        return;
    }
    let tl_rate = engine.project().timeline().frame_rate;
    // Parking starts past everything on any lane; spaced by each member's
    // duration so parked members can't collide either.
    let mut park = engine
        .project()
        .timeline()
        .tracks_ordered()
        .map(|t| t.content_end())
        .max()
        .unwrap_or(0);

    engine.begin_group();
    for &(clip_id, to_track, _, _) in &resolved {
        let duration = engine
            .project()
            .clip(clip_id)
            .map(|c| c.timeline.duration.value)
            .unwrap_or(1);
        if let Err(e) = apply_edit(
            engine,
            EditCommand::MoveClip {
                clip: clip_id,
                to_track,
                start: RationalTime::new(park, tl_rate),
            },
        ) {
            error!(%clip_id, %to_track, "group move failed parking: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
        park += duration;
    }
    for &(clip_id, to_track, _, start_tick) in &resolved {
        if let Err(e) = apply_edit(
            engine,
            EditCommand::MoveClip {
                clip: clip_id,
                to_track,
                start: RationalTime::new(start_tick, tl_rate),
            },
        ) {
            error!(%clip_id, %to_track, start_tick, "group move failed landing: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    // Lane cleanup after all landings (dedupe: one check per source lane).
    let mut sources: Vec<TrackId> = resolved.iter().map(|&(_, _, source, _)| source).collect();
    sources.sort();
    sources.dedup();
    for source in sources {
        remove_track_if_empty(engine, source);
    }
    engine.commit_group();
    info!(count = resolved.len(), "moved clip group");
    publish_projection(engine, ui);
}

/// Step the engine history (`redo == false` ⇒ undo). Publishes even on a
/// no-op so the UI's can-undo / can-redo flags stay honest.
fn history_step_and_publish(engine: &mut Engine, redo: bool, ui: &UiSink) {
    let stepped = if redo { engine.redo() } else { engine.undo() };
    info!(redo, stepped, "history step");
    publish_projection(engine, ui);
}

/// Snapshot `clips` (raw ids — the selection) as one clipboard block:
/// members in start order, offsets rebased to the earliest start. Returns
/// the block origin (that earliest start) alongside, for callers that place
/// relative to the originals (duplicate). Ids that no longer resolve are
/// skipped; an empty result is `None`.
fn snapshot_block(engine: &Engine, clips: &[String]) -> Option<(i64, Vec<ClipboardClip>)> {
    let timeline = engine.project().timeline();
    let mut members = Vec::with_capacity(clips.len());
    for raw in clips {
        let Some(clip_id) = parse_raw_id(raw).map(ClipId::from_raw) else {
            continue;
        };
        let Some(track) = timeline.track_of(clip_id) else {
            continue;
        };
        let Some(kind) = timeline.track(track).map(|t| t.kind) else {
            continue;
        };
        let Some(clip) = engine.project().clip(clip_id) else {
            continue;
        };
        members.push(ClipboardClip {
            track,
            kind,
            content: clip.content.clone(),
            duration_ticks: clip.timeline.duration.value,
            // Absolute start for now; rebased to the block origin below.
            offset_ticks: clip.timeline.start.value,
            link: clip.link,
        });
    }
    if members.is_empty() {
        return None;
    }
    members.sort_by_key(|m| m.offset_ticks);
    let origin = members[0].offset_ticks;
    for member in &mut members {
        member.offset_ticks -= origin;
    }
    Some((origin, members))
}

/// Smallest uniform right-shift (≥ 0) that lets every `(lane, start,
/// duration)` span land without overlapping existing clips. Members can't
/// collide with each other (a uniform shift preserves their relative,
/// originally disjoint placement), so only 0 and the "blocked member
/// becomes left-flush against an existing clip's end" shifts can be the
/// minimum — the group analogue of `first_fit_start`'s gap scan. O(n·m)
/// per candidate on this cold, user-triggered path.
fn block_fit_dx(engine: &Engine, spans: &[(TrackId, i64, i64)]) -> i64 {
    let timeline = engine.project().timeline();
    let fits = |dx: i64| {
        spans.iter().all(|&(track, start, duration)| {
            timeline
                .track(track)
                .is_some_and(|t| span_free(t, start + dx, duration))
        })
    };
    let mut candidates: Vec<i64> = vec![0];
    for &(track, start, _) in spans {
        let Some(track) = timeline.track(track) else {
            continue;
        };
        for clip in track.clips_ordered() {
            let dx = clip.timeline.end_tick() - start;
            if dx > 0 {
                candidates.push(dx);
            }
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    // The largest candidate parks every member at/after the last clip on
    // its lane, so a fit always exists; 0 covers the all-lanes-empty case.
    candidates.into_iter().find(|&dx| fits(dx)).unwrap_or(0)
}

/// Place every member of a resolved block — `(landing lane, desired start,
/// member)` — inside the caller's open history group: one uniform
/// right-shift until everything fits, then re-issue each member's content
/// and re-link copies whose originals shared a link group (singleton
/// leftovers of partially copied groups stay unlinked).
fn place_block(
    engine: &mut Engine,
    members: &[(TrackId, i64, &ClipboardClip)],
) -> Result<(), String> {
    let spans: Vec<(TrackId, i64, i64)> = members
        .iter()
        .map(|&(track, start, member)| (track, start, member.duration_ticks.max(1)))
        .collect();
    let dx = block_fit_dx(engine, &spans);

    let mut created: Vec<(Option<LinkId>, ClipId)> = Vec::with_capacity(members.len());
    for &(track, start, member) in members {
        let id = add_clip_content(
            engine,
            track,
            &member.content,
            member.duration_ticks,
            start + dx,
        )?;
        created.push((member.link, id));
    }

    let mut seen: Vec<LinkId> = Vec::new();
    for &(link, _) in &created {
        let Some(link) = link else { continue };
        if seen.contains(&link) {
            continue;
        }
        seen.push(link);
        let group: Vec<ClipId> = created
            .iter()
            .filter(|(l, _)| *l == Some(link))
            .map(|&(_, id)| id)
            .collect();
        if group.len() >= 2 {
            apply_edit(engine, EditCommand::LinkClips { clips: group })?;
        }
    }
    Ok(())
}

/// Paste the clipboard block at `tick`: members land on the lanes they were
/// copied from (recreated by kind when gone), keeping relative placement;
/// the whole block slides right as one unit until every member fits — the
/// group analogue of the library-drop policy. A single-member block keeps
/// the magnet behavior: pasted on the main lane with the magnet on, it
/// ripple-inserts at the clip boundary nearest `tick` instead (groups stay
/// freeform, same policy as group drags).
fn paste_and_publish(
    engine: &mut Engine,
    block: &[ClipboardClip],
    tick: i64,
    main_magnet: bool,
    ui: &UiSink,
) {
    let tl_rate = engine.project().timeline().frame_rate;

    // One history entry per paste, even when it recreates copied lanes.
    engine.begin_group();

    // Landing lane per source lane: the original when it still exists, one
    // fresh lane of its kind (top of the stack, as single-paste always did)
    // per vanished track id.
    let mut lanes: HashMap<TrackId, TrackId> = HashMap::new();
    for member in block {
        if lanes.contains_key(&member.track) {
            continue;
        }
        let landing = if engine.project().timeline().track(member.track).is_some() {
            member.track
        } else {
            match create_track(engine, member.kind, 0) {
                Ok(id) => id,
                Err(e) => {
                    error!("paste failed creating {:?} track: {e}", member.kind);
                    engine.rollback_group();
                    return;
                }
            }
        };
        lanes.insert(member.track, landing);
    }

    // Single-clip ripple-insert (magnet) keeps its dedicated path.
    if let [only] = block {
        let track = lanes[&only.track];
        if main_magnet && Some(track) == main_video_track(engine) {
            let duration = only.duration_ticks.max(1);
            let lane = engine
                .project()
                .timeline()
                .track(track)
                .expect("paste target track exists");
            let start = nearest_boundary(lane, tick.max(0));
            let result = apply_edit(
                engine,
                EditCommand::ShiftClips {
                    track,
                    from: RationalTime::new(start, tl_rate),
                    delta: RationalTime::new(duration, tl_rate),
                },
            )
            .and_then(|_| {
                add_clip_content(engine, track, &only.content, only.duration_ticks, start)
            });
            match result {
                Ok(clip_id) => {
                    engine.commit_group();
                    info!(%clip_id, %track, start_tick = start, "ripple-pasted clip");
                }
                Err(e) => {
                    error!(%track, start_tick = start, "paste failed: {e}");
                    engine.rollback_group();
                }
            }
            publish_projection(engine, ui);
            return;
        }
    }

    let members: Vec<(TrackId, i64, &ClipboardClip)> = block
        .iter()
        .map(|member| {
            (
                lanes[&member.track],
                tick.max(0) + member.offset_ticks,
                member,
            )
        })
        .collect();
    match place_block(engine, &members) {
        Ok(()) => {
            engine.commit_group();
            info!(count = block.len(), tick, "pasted clipboard block");
        }
        // Rolling back also removes lanes this paste just recreated.
        Err(e) => {
            error!(tick, "paste failed: {e}");
            engine.rollback_group();
        }
    }
    publish_projection(engine, ui);
}

/// Duplicate the selection as one block: copies keep their lanes and
/// relative placement, landing right after the block's end — slid further
/// right as one unit when something is in the way. Copies of linked members
/// re-link as fresh groups; one history entry for everything. Freeform like
/// group drags (no group ripple-insert) — a single clip keeps the
/// magnet-aware single-duplicate path below.
fn duplicate_clips_and_publish(
    engine: &mut Engine,
    clips: &[String],
    main_magnet: bool,
    ui: &UiSink,
) {
    if let [only] = clips {
        duplicate_clip_and_publish(engine, only, main_magnet, ui);
        return;
    }
    let Some((origin, block)) = snapshot_block(engine, clips) else {
        info!("duplicate ignored: no valid clips in selection");
        return;
    };
    let span = block
        .iter()
        .map(|m| m.offset_ticks + m.duration_ticks.max(1))
        .max()
        .unwrap_or(1);
    // Copies land right after the originals' span; lanes all exist (the
    // originals are live), so no lane resolution is needed.
    let base = origin + span;
    let members: Vec<(TrackId, i64, &ClipboardClip)> = block
        .iter()
        .map(|member| (member.track, base + member.offset_ticks, member))
        .collect();

    engine.begin_group();
    match place_block(engine, &members) {
        Ok(()) => {
            engine.commit_group();
            info!(count = block.len(), "duplicated clip block");
        }
        Err(e) => {
            error!("duplicate failed: {e}");
            engine.rollback_group();
        }
    }
    publish_projection(engine, ui);
}

/// Dissolve the link groups of `clips` (raw ids): every member of every
/// touched group — selected or not — ends up unlinked. Implemented with the
/// existing `LinkClips` command by giving each member a fresh *singleton*
/// group, which behaves exactly like no link everywhere links are read
/// (selection expansion, linked trims/splits, drops). One history entry;
/// undo restores the old groups (the link action snapshots prior values).
/// A dedicated `UnlinkClips` (link = None) can replace the singleton trick
/// once the command surface is open again post-M1.
fn unlink_clips_and_publish(engine: &mut Engine, clips: &[String], ui: &UiSink) {
    // Link ids represented in the selection…
    let mut links: Vec<LinkId> = Vec::new();
    for raw in clips {
        let Some(clip_id) = parse_raw_id(raw).map(ClipId::from_raw) else {
            continue;
        };
        if let Some(link) = engine.project().clip(clip_id).and_then(|c| c.link)
            && !links.contains(&link)
        {
            links.push(link);
        }
    }
    if links.is_empty() {
        info!("unlink ignored: selection has no linked clips");
        return;
    }
    // …expanded to full membership, so groups dissolve as a whole.
    let members: Vec<ClipId> = engine
        .project()
        .timeline()
        .tracks_ordered()
        .flat_map(|t| t.clips_ordered())
        .filter(|c| c.link.is_some_and(|l| links.contains(&l)))
        .map(|c| c.id)
        .collect();

    engine.begin_group();
    for member in &members {
        if let Err(e) = apply_edit(
            engine,
            EditCommand::LinkClips {
                clips: vec![*member],
            },
        ) {
            error!(%member, "unlink failed: {e}");
            engine.rollback_group();
            publish_projection(engine, ui);
            return;
        }
    }
    engine.commit_group();
    info!(
        groups = links.len(),
        members = members.len(),
        "unlinked clip groups"
    );
    publish_projection(engine, ui);
}

/// Place a copy of `clip` immediately after it on its own lane (first gap
/// that fits from the clip's end). With the main-track magnet on, a main-lane
/// duplicate ripple-inserts right after the original, shifting later clips.
fn duplicate_clip_and_publish(engine: &mut Engine, clip: &str, main_magnet: bool, ui: &UiSink) {
    let Some(clip_id) = parse_raw_id(clip).map(ClipId::from_raw) else {
        error!(clip, "duplicate ignored: unparsable clip id");
        return;
    };
    let Some(track) = engine.project().timeline().track_of(clip_id) else {
        error!(%clip_id, "duplicate ignored: clip not on the timeline");
        return;
    };
    let original = engine
        .project()
        .clip(clip_id)
        .expect("track_of returned a placed clip");
    let content = original.content.clone();
    let duration_ticks = original.timeline.duration.value.max(1);
    let end_tick = original.timeline.end_tick();
    let tl_rate = engine.project().timeline().frame_rate;

    if main_magnet && Some(track) == main_video_track(engine) {
        // Open a hole right after the original, land the copy in it — one
        // history entry for the pair.
        engine.begin_group();
        let result = apply_edit(
            engine,
            EditCommand::ShiftClips {
                track,
                from: RationalTime::new(end_tick, tl_rate),
                delta: RationalTime::new(duration_ticks, tl_rate),
            },
        )
        .and_then(|_| add_clip_content(engine, track, &content, duration_ticks, end_tick));
        match result {
            Ok(copy_id) => {
                engine.commit_group();
                info!(%clip_id, %copy_id, %track, start_tick = end_tick, "ripple-duplicated clip");
            }
            Err(e) => {
                error!(%clip_id, start_tick = end_tick, "duplicate failed: {e}");
                engine.rollback_group();
            }
        }
        publish_projection(engine, ui);
        return;
    }

    let lane = engine
        .project()
        .timeline()
        .track(track)
        .expect("track_of returned an existing track");
    let start = first_fit_start(lane, end_tick, duration_ticks);

    match add_clip_content(engine, track, &content, duration_ticks, start) {
        Ok(copy_id) => {
            info!(%clip_id, %copy_id, %track, start_tick = start, "duplicated clip");
            publish_projection(engine, ui);
        }
        Err(e) => error!(%clip_id, start_tick = start, "duplicate failed: {e}"),
    }
}

/// Close every gap on the main lane, including leading space before the
/// first clip — CapCut's lane is gapless the moment the magnet turns on.
/// One history group: a single undo restores the gaps.
fn pack_main_track_and_publish(engine: &mut Engine, ui: &UiSink) {
    let Some(track) = main_video_track(engine) else {
        return;
    };
    let tl_rate = engine.project().timeline().frame_rate;
    // (start, duration) snapshot in start order. Each shift slides the whole
    // suffix left, so positions after it are tracked via the running offset
    // instead of re-reading the engine.
    let clips: Vec<(i64, i64)> = engine
        .project()
        .timeline()
        .track(track)
        .map(|t| {
            t.clips_ordered()
                .iter()
                .map(|c| (c.timeline.start.value, c.timeline.duration.value))
                .collect()
        })
        .unwrap_or_default();

    let mut shifted_so_far = 0;
    let mut expected = 0;
    engine.begin_group();
    for (start, duration) in clips {
        let current = start - shifted_so_far;
        if current > expected {
            if let Err(e) = apply_edit(
                engine,
                EditCommand::ShiftClips {
                    track,
                    from: RationalTime::new(current, tl_rate),
                    delta: RationalTime::new(expected - current, tl_rate),
                },
            ) {
                error!(%track, "magnet pack failed: {e}");
                engine.rollback_group();
                publish_projection(engine, ui);
                return;
            }
            shifted_so_far += current - expected;
        }
        expected += duration;
    }
    // An already-packed lane records nothing (empty groups are dropped).
    engine.commit_group();
    publish_projection(engine, ui);
}

/// The main track under CapCut's magnet: the video lane the model designates
/// (`Track::main` — the timeline keeps it directly above the audio floor).
fn main_video_track(engine: &Engine) -> Option<TrackId> {
    engine.project().timeline().main_track()
}

/// Clip boundary on `track` nearest to `tick`: every clip start plus the
/// content end (0 on an empty lane). Ties resolve to the earlier boundary.
fn nearest_boundary(track: &Track, tick: i64) -> i64 {
    let mut best = 0;
    let mut best_distance = i64::MAX;
    let mut consider = |boundary: i64| {
        let distance = (tick - boundary).abs();
        if distance < best_distance {
            best = boundary;
            best_distance = distance;
        }
    };
    for clip in track.clips_ordered() {
        consider(clip.timeline.start.value);
    }
    consider(track.content_end());
    best
}

/// Replay a rehearsed agent plan on the live engine, re-validated step by
/// step, with sandbox-allocated ids remapped onto the ids the live engine
/// hands out. Each phase (see `PromptOutcome::phase_breaks`) commits as
/// its own history group — one undo step per phase. `after_step` runs
/// after every applied step (the worker publishes there, so the user
/// watches the plan land) and after the final commit or a rollback. A
/// failure rolls back the failing phase only and stops: earlier phases
/// were valid and committed, so they stay, each independently undoable —
/// the error says how many landed. Id maps persist across phases (a later
/// phase may address a clip an earlier one created). Each phase also
/// enforces the desktop's empty-lane invariant before it commits; a phase
/// checkpoint therefore must leave a coherent timeline on its own.
pub(crate) fn agent_replay(
    engine: &mut Engine,
    phases: Vec<Vec<AgentPlanStep>>,
    mut after_step: impl FnMut(&mut Engine),
) -> Result<(), String> {
    use std::collections::HashMap as Map;
    let mut clip_map: Map<u64, u64> = Map::new();
    let mut track_map: Map<u64, u64> = Map::new();
    let mut marker_map: Map<u64, u64> = Map::new();
    let phase_count = phases.len();
    let mut applied = 0usize;
    for (phase_index, steps) in phases.into_iter().enumerate() {
        let total = steps.len();
        let mut cleanup_lanes = Vec::new();
        engine.begin_group();
        for (index, mut step) in steps.into_iter().enumerate() {
            step.command.remap_ids(&clip_map, &track_map, &marker_map);
            let cleanup_lane = agent_cleanup_source_lane(engine, &step.command);
            let outcome = cutlass_ai::validate(&step.command, engine.project())
                .map_err(|r| r.message)
                .and_then(|lowered| engine.apply(lowered).map_err(|e| e.to_string()));
            match outcome {
                Ok(ApplyOutcome::Edited(edited)) => {
                    if let Some(lane) = cleanup_lane {
                        cleanup_lanes.push(lane);
                    }
                    match (step.created, &edited) {
                        (Some(AgentCreated::Clip(sandbox)), EditOutcome::Created(live)) => {
                            clip_map.insert(sandbox, live.raw());
                        }
                        (Some(AgentCreated::Track(sandbox)), EditOutcome::CreatedTrack(live)) => {
                            track_map.insert(sandbox, live.raw());
                        }
                        (Some(AgentCreated::Marker(sandbox)), EditOutcome::CreatedMarker(live)) => {
                            marker_map.insert(sandbox, live.raw());
                        }
                        _ => {}
                    }
                    after_step(engine);
                }
                Ok(other) => {
                    engine.rollback_group();
                    after_step(engine);
                    return Err(replay_failure(
                        phase_index,
                        phase_count,
                        index,
                        total,
                        &format!("unexpected engine outcome {other:?}"),
                    ));
                }
                Err(reason) => {
                    engine.rollback_group();
                    after_step(engine);
                    return Err(replay_failure(
                        phase_index,
                        phase_count,
                        index,
                        total,
                        &reason,
                    ));
                }
            }
        }
        // Agent commands bypass the desktop gesture helpers, so mirror
        // their CapCut lane policy at every commit boundary. A phase is an
        // independently undoable state and must satisfy desktop invariants;
        // if a plan wants to empty and refill a lane, those edits belong in
        // the same phase.
        cleanup_lanes.sort();
        cleanup_lanes.dedup();
        for lane in cleanup_lanes {
            remove_track_if_empty(engine, lane);
        }
        engine.commit_group();
        applied += total;
    }
    info!(steps = applied, phases = phase_count, "agent plan applied");
    after_step(engine);
    Ok(())
}

/// Replay failures name the phase (when there are several), the failing
/// step, and how much of the plan already landed — the transcript line
/// must let the user judge what a failure left behind.
fn replay_failure(
    phase_index: usize,
    phase_count: usize,
    step_index: usize,
    steps_in_phase: usize,
    reason: &str,
) -> String {
    let step = format!("step {}/{}", step_index + 1, steps_in_phase);
    let location = if phase_count > 1 {
        format!("phase {}/{} {step}", phase_index + 1, phase_count)
    } else {
        step
    };
    let landed = match phase_index {
        0 => "nothing was applied".to_string(),
        1 => format!("phase 1 of {phase_count} was applied and stays undoable"),
        n => format!("phases 1–{n} of {phase_count} were applied and stay undoable"),
    };
    format!("{location}: {reason} — {landed}")
}

/// Source lane that may become empty after an agent structural edit.
fn agent_cleanup_source_lane(
    engine: &Engine,
    command: &cutlass_ai::WireCommand,
) -> Option<TrackId> {
    let clip = match command {
        cutlass_ai::WireCommand::MoveClip(args) => args.clip,
        cutlass_ai::WireCommand::RemoveClip(args) => args.clip,
        cutlass_ai::WireCommand::RippleDelete(args) => args.clip,
        _ => return None,
    };
    engine.project().timeline().track_of(ClipId::from_raw(clip))
}

fn agent_apply_and_publish(
    engine: &mut Engine,
    phases: Vec<Vec<AgentPlanStep>>,
    ui: &UiSink,
) -> Result<(), String> {
    agent_replay(engine, phases, |engine| publish_projection(engine, ui))
}

/// Apply a single edit command, flattening the outcome — for compositions
/// where only success/failure matters (the group publishes once at the end).
fn apply_edit(engine: &mut Engine, command: EditCommand) -> Result<(), String> {
    engine
        .apply(Command::Edit(command))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Re-issue snapshotted clip content as a fresh engine command: `AddClip`
/// for media-backed content, `AddGenerated` for generated content.
fn add_clip_content(
    engine: &mut Engine,
    track: TrackId,
    content: &ClipSource,
    duration_ticks: i64,
    start_tick: i64,
) -> Result<ClipId, String> {
    let tl_rate = engine.project().timeline().frame_rate;
    let command = match content {
        ClipSource::Media { media, source } => EditCommand::AddClip {
            track,
            media: *media,
            source: *source,
            start: RationalTime::new(start_tick, tl_rate),
        },
        ClipSource::Generated(generator) => EditCommand::AddGenerated {
            track,
            generator: generator.clone(),
            timeline: TimeRange::at_rate(start_tick, duration_ticks.max(1), tl_rate),
        },
    };
    match engine.apply(Command::Edit(command)) {
        Ok(ApplyOutcome::Edited(EditOutcome::Created(id))) => Ok(id),
        Ok(other) => Err(format!("unexpected add outcome: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Remove `track` when an edit left it empty (CapCut removes emptied lanes).
/// The main track is exempt: it's the one lane that exists without clips.
fn remove_track_if_empty(engine: &mut Engine, track: TrackId) {
    let emptied = engine
        .project()
        .timeline()
        .track(track)
        .is_some_and(|t| t.is_empty() && !t.pinned && !t.main);
    if !emptied {
        return;
    }
    if let Err(e) = engine.apply(Command::Edit(EditCommand::RemoveTrack { track })) {
        error!(%track, "failed to remove emptied track: {e}");
    }
}

/// Create a new track of `kind` for drops/moves that don't target an existing
/// lane, inserted so it appears at `drop_row` in the lane list. Named by
/// kind + per-kind count (V1, V2, A1, …).
fn create_track(engine: &mut Engine, kind: TrackKind, drop_row: i64) -> Result<TrackId, String> {
    let timeline = engine.project().timeline();
    // The lane list shows the stack top-first (see projection.rs), so the new
    // lane appears at UI row r when inserted at stack index (len - r). The
    // clamp covers drops above the first lane (⇒ top of stack) and below the
    // last (⇒ bottom).
    let stack_len = timeline.order().len() as i64;
    let order_index = (stack_len - drop_row).clamp(0, stack_len) as usize;
    let count = timeline.tracks_ordered().filter(|t| t.kind == kind).count();
    match engine.apply(Command::Edit(EditCommand::AddTrack {
        kind,
        name: format!("{}{}", kind_prefix(kind), count + 1),
        index: Some(order_index),
        pinned: false,
    })) {
        Ok(ApplyOutcome::Edited(EditOutcome::CreatedTrack(id))) => Ok(id),
        Ok(other) => Err(format!("unexpected add-track outcome: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

fn kind_prefix(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "V",
        TrackKind::Audio => "A",
        TrackKind::Text => "T",
        TrackKind::Sticker => "ST",
        TrackKind::Effect => "FX",
        TrackKind::Filter => "F",
        TrackKind::Adjustment => "ADJ",
    }
}

/// First start ≥ `desired` where `[start, start + duration)` fits in a gap on
/// `track`. Clips are scanned in start order (they never overlap), so a blocked
/// candidate just slides to the blocker's end — O(n) on this cold per-drop path.
fn first_fit_start(track: &Track, desired: i64, duration_ticks: i64) -> i64 {
    let mut start = desired;
    for clip in track.clips_ordered() {
        if start + duration_ticks <= clip.timeline.start.value {
            break; // fits entirely before this clip
        }
        start = start.max(clip.timeline.end_tick());
    }
    start
}

fn parse_raw_id(raw: &str) -> Option<u64> {
    raw.parse().ok()
}

/// Snapshot the engine's project and hand it to the UI thread, which rebuilds
/// the Slint view model. The snapshot crosses the thread boundary (`Send`);
/// the `!Send` Slint model types are constructed inside the event-loop closure.
/// History availability rides along so the toolbar's undo/redo states always
/// match the projection they were published with.
///
/// PORT (Phase 3): the audio mixer's timeline snapshot publishes from this
/// same chokepoint on main, so what playback sounds like can never diverge
/// from what the UI shows.
fn publish_projection(engine: &mut Engine, ui: &UiSink) {
    let generator_sizes = generator_content_sizes(engine);
    // Pool entries whose backing file is gone (raw ids) — drives the relink
    // dialog count and the library tiles' missing badges. Computed here on
    // the worker thread so the UI thread never stats the filesystem (a dead
    // network mount must not hitch painting).
    let missing_media: std::collections::HashSet<u64> = engine
        .project()
        .media_iter()
        .filter(|m| !m.path().exists())
        .map(|m| m.id.raw())
        .collect();
    let project = engine.project().clone();
    // The audio mixer hears every edit through the same chokepoint that
    // republishes the view model, so sound and picture can't drift apart.
    ui.audio.publish_snapshot(project.clone());
    let can_undo = engine.can_undo();
    let can_redo = engine.can_redo();
    // Session save state rides the same chokepoint as the project view, so
    // the title bar's dirty dot can never disagree with the engine.
    let dirty = engine.is_dirty();
    let file_name = engine
        .project_path()
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let has_path = engine.project_path().is_some();
    // Full path (not just the stem): main.rs needs it to address the
    // session's autosave slot when a close discards unsaved work.
    let file_path = engine
        .project_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let editor_weak = ui.editor.clone();
    if let Err(e) = slint::invoke_from_event_loop(move || {
        if let Some(store) = editor_weak.upgrade() {
            store.set_project(crate::projection::project_to_slint(
                &project,
                &generator_sizes,
                &missing_media,
            ));
            store.set_missing_media_count(missing_media.len() as i32);
            store.set_can_undo(can_undo);
            store.set_can_redo(can_redo);
            store.set_projection_revision(store.get_projection_revision().saturating_add(1));
            store.set_project_dirty(dirty);
            store.set_project_has_path(has_path);
            store.set_project_file_name(file_name.into());
            store.set_project_file_path(file_path.into());
        }
    }) {
        error!("failed to publish project projection to UI: {e}");
    }
}

/// Bump `EditorStore.session-epoch`: the session was replaced wholesale
/// (open / new), and UI-side session state — playhead, selection, in/out
/// range, playback — must reset. The watcher lives in `app.slint`.
fn bump_session_epoch(ui: &UiSink) {
    let editor_weak = ui.editor.clone();
    if let Err(e) = slint::invoke_from_event_loop(move || {
        if let Some(store) = editor_weak.upgrade() {
            store.set_session_epoch(store.get_session_epoch() + 1);
        }
    }) {
        error!("failed to bump session epoch: {e}");
    }
}

/// Surface a session-level failure (save/open) to the user: sets
/// `EditorStore.session-error`, which mounts the message dialog until the
/// user dismisses it (clearing the property).
fn publish_session_error(ui: &UiSink, message: String) {
    let editor_weak = ui.editor.clone();
    if let Err(e) = slint::invoke_from_event_loop(move || {
        if let Some(store) = editor_weak.upgrade() {
            store.set_session_error(message.into());
        }
    }) {
        error!("failed to publish session error: {e}");
    }
}

/// Drawn-content size (canvas px) for every generated clip, keyed by raw clip
/// id — the preview's selection box and hit-test hug what the generator
/// actually draws instead of its full-canvas raster. Served from the engine's
/// raster caches; clips the compositor doesn't draw are absent (the UI falls
/// back to canvas size). Animated params are sampled at the clip's first
/// frame — the projection republishes per edit, not per playhead move, so a
/// single representative size is all it can carry.
fn generator_content_sizes(engine: &mut Engine) -> HashMap<u64, (i32, i32)> {
    let generators: Vec<(u64, Generator)> = engine
        .project()
        .timeline()
        .tracks_ordered()
        .flat_map(|track| track.clips())
        .filter_map(|clip| match &clip.content {
            ClipSource::Generated(generator) => Some((clip.id.raw(), generator.clone())),
            ClipSource::Media { .. } => None,
        })
        .collect();
    generators
        .into_iter()
        .filter_map(|(id, generator)| {
            let (w, h) = engine.generator_content_size(&generator, 0)?;
            Some((id, (w as i32, h as i32)))
        })
        .collect()
}

// PORT (Phase 3): main built the playback mixer's `AudioSnapshot` here (every
// audible clip with its source window, retime, volume envelope, and fades).
// It returns with the cpal audio system on `ExportAudioMixer`.
