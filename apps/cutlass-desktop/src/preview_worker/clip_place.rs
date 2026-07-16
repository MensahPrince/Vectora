use super::*;

/// Rename the current draft (title bar). Applied as one undoable edit so it
/// joins the undo history and dirties the session; the projection republish
/// updates the title, and the debounced auto-save writes the new name into
/// the draft's project file and meta sidecar.
pub(super) fn rename_project_and_publish(engine: &mut Engine, name: String, ui: &UiSink) {
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
pub(super) fn add_clip_and_publish(
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
pub(super) fn add_generated_and_publish(
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
