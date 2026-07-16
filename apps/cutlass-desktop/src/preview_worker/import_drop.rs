use super::*;

pub(super) fn import_and_publish(engine: &mut Engine, path: &Path, ui: &UiSink) {
    let _ = import_media_rpc_and_publish(engine, path, Some(ui));
}

/// Shared import implementation for fire-and-forget UI work and acknowledged
/// RPCs. `ui` is `None` only in engine-level unit tests.
pub(super) fn import_media_rpc_and_publish(
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
pub(super) struct DroppedMedia {
    pub(super) media: MediaId,
    pub(super) source: TimeRange,
    pub(super) duration_ticks: i64,
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
pub(super) fn drop_files_and_publish(
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
pub(super) fn place_drop_group(
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
pub(super) fn track_at_row(engine: &Engine, row: i64) -> Option<&Track> {
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
pub(super) fn occupied_spans(track: &Track) -> Vec<(i64, i64)> {
    track
        .clips_ordered()
        .iter()
        .map(|c| (c.timeline.start.value, c.timeline.end_tick()))
        .collect()
}
