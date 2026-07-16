use super::*;

/// Retime a media clip (CapCut speed, M1). The engine validates (positive
/// speed, media-backed clip, no neighbor overlap) and re-derives the
/// timeline duration; one undoable history entry. With linkage on, the
/// clip's link partners (the video+audio pair from one media drop) retime
/// together in one history group, so the pair stays in sync and one undo
/// restores both. Audio of retimed clips is muted by the snapshot builder,
/// so the republish silences it immediately.
pub(super) fn set_clip_speed_and_publish(
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
pub(super) fn set_clip_pitch_and_publish(
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
pub(super) fn set_denoise_and_publish(
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
pub(super) fn set_speed_curve_and_publish(
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
pub(super) fn set_speed_curve_point_and_publish(
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
