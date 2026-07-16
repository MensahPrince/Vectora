use super::*;

/// Remove every clip in `clips`; lanes the removals empty are removed with
/// them (CapCut deletes emptied overlay tracks — same policy the drag-moves
/// use). With the main-track magnet on, main-lane deletions ripple their
/// gaps closed. Everything forms one history group: one undo restores the
/// whole selection.
pub(super) fn remove_clips_and_publish(
    engine: &mut Engine,
    clips: &[String],
    main_magnet: bool,
    ui: &UiSink,
) {
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
pub(super) fn ripple_delete_clips_and_publish(engine: &mut Engine, clips: &[String], ui: &UiSink) {
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
pub(super) fn reverse_clip_and_publish(
    engine: &mut Engine,
    clip: &str,
    linkage: bool,
    ui: &UiSink,
) {
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
pub(super) fn extract_audio_and_publish(engine: &mut Engine, clip: &str, ui: &UiSink) {
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
pub(super) fn extract_audio(engine: &mut Engine, clip_id: ClipId) -> Result<ClipId, String> {
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
pub(super) fn split_clip_and_publish(
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
pub(super) fn move_group_and_publish(engine: &mut Engine, moves: &[GroupMove], ui: &UiSink) {
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
pub(super) fn history_step_and_publish(engine: &mut Engine, redo: bool, ui: &UiSink) {
    let stepped = if redo { engine.redo() } else { engine.undo() };
    info!(redo, stepped, "history step");
    publish_projection(engine, ui);
}

/// Snapshot `clips` (raw ids — the selection) as one clipboard block:
/// members in start order, offsets rebased to the earliest start. Returns
/// the block origin (that earliest start) alongside, for callers that place
/// relative to the originals (duplicate). Ids that no longer resolve are
/// skipped; an empty result is `None`.
pub(super) fn snapshot_block(
    engine: &Engine,
    clips: &[String],
) -> Option<(i64, Vec<ClipboardClip>)> {
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
pub(super) fn block_fit_dx(engine: &Engine, spans: &[(TrackId, i64, i64)]) -> i64 {
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
pub(super) fn place_block(
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
pub(super) fn paste_and_publish(
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
pub(super) fn duplicate_clips_and_publish(
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
pub(super) fn unlink_clips_and_publish(engine: &mut Engine, clips: &[String], ui: &UiSink) {
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
pub(super) fn duplicate_clip_and_publish(
    engine: &mut Engine,
    clip: &str,
    main_magnet: bool,
    ui: &UiSink,
) {
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
pub(super) fn pack_main_track_and_publish(engine: &mut Engine, ui: &UiSink) {
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
