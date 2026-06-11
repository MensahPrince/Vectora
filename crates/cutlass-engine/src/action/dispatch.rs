use cutlass_commands::{Command, EditCommand, EditOutcome, ProjectCommand};

use super::edit::add_track::RemoveTrackAction;
use super::edit::{self, remove_clip::RemoveClipAction};
use super::project::{self, import};
use super::{ApplyContext, EditAction};
use crate::error::EngineError;

/// Result of applying a wire [`Command`] through the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    Imported { media: cutlass_models::MediaId },
    Saved,
    Opened,
    Loaded,
    Exported { stats: cutlass_encoder::ExportStats },
    Edited(EditOutcome),
}

pub fn dispatch(
    command: Command,
    ctx: &mut ApplyContext<'_>,
) -> Result<(ApplyOutcome, Option<Box<dyn EditAction>>), EngineError> {
    match command {
        Command::Project(project) => dispatch_project(project, ctx),
        Command::Edit(edit) => dispatch_edit(edit, ctx),
    }
}

fn dispatch_project(
    command: ProjectCommand,
    ctx: &mut ApplyContext<'_>,
) -> Result<(ApplyOutcome, Option<Box<dyn EditAction>>), EngineError> {
    match command {
        ProjectCommand::Import { path } => {
            let (media, inverse) = import::execute(ctx, &path)?;
            Ok((ApplyOutcome::Imported { media }, inverse))
        }
        ProjectCommand::Save { path } => {
            project::save::execute(ctx, path)?;
            Ok((ApplyOutcome::Saved, None))
        }
        ProjectCommand::Open { path } => {
            project::open::execute(ctx, path)?;
            Ok((ApplyOutcome::Opened, None))
        }
        ProjectCommand::Load { path } => {
            project::load::execute(ctx, path)?;
            Ok((ApplyOutcome::Loaded, None))
        }
        ProjectCommand::Export { .. } => Err(EngineError::Export(
            "export is handled by Engine::apply, not dispatch".into(),
        )),
    }
}

fn dispatch_edit(
    edit: EditCommand,
    ctx: &mut ApplyContext<'_>,
) -> Result<(ApplyOutcome, Option<Box<dyn EditAction>>), EngineError> {
    match edit {
        EditCommand::AddTrack { kind, name, index } => {
            let (id, inverse) = edit::add_track::execute(ctx, kind, name, index)?;
            Ok((ApplyOutcome::Edited(EditOutcome::CreatedTrack(id)), Some(inverse)))
        }
        EditCommand::AddClip {
            track,
            media,
            source,
            start,
        } => {
            let (id, inverse) = edit::add_clip::execute(ctx, track, media, source, start)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Created(id)), Some(inverse)))
        }
        EditCommand::AddGenerated {
            track,
            generator,
            timeline,
        } => {
            let (id, inverse) = edit::add_generated::execute(ctx, track, generator, timeline)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Created(id)), Some(inverse)))
        }
        EditCommand::SplitClip { clip, at } => {
            let (id, inverse) = edit::split_clip::execute(ctx, clip, at)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Created(id)), Some(inverse)))
        }
        EditCommand::TrimClip { clip, timeline } => {
            let inverse = edit::trim_clip::execute(ctx, clip, timeline)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Updated(clip)), Some(inverse)))
        }
        EditCommand::MoveClip {
            clip,
            to_track,
            start,
        } => {
            let inverse = edit::move_clip::execute(ctx, clip, to_track, start)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Updated(clip)), Some(inverse)))
        }
        EditCommand::RemoveClip { clip } => {
            let inverse = Box::new(RemoveClipAction { clip }).apply(ctx)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Removed(clip)), Some(inverse)))
        }
        EditCommand::RemoveTrack { track } => {
            let inverse = Box::new(RemoveTrackAction { track_id: track }).apply(ctx)?;
            Ok((ApplyOutcome::Edited(EditOutcome::RemovedTrack(track)), Some(inverse)))
        }
        EditCommand::SetTrackEnabled { track, enabled } => {
            let inverse = edit::set_track_flags::execute(ctx, track, Some(enabled), None, None)?;
            Ok((ApplyOutcome::Edited(EditOutcome::UpdatedTrack(track)), Some(inverse)))
        }
        EditCommand::SetTrackMuted { track, muted } => {
            let inverse = edit::set_track_flags::execute(ctx, track, None, Some(muted), None)?;
            Ok((ApplyOutcome::Edited(EditOutcome::UpdatedTrack(track)), Some(inverse)))
        }
        EditCommand::SetTrackLocked { track, locked } => {
            let inverse = edit::set_track_flags::execute(ctx, track, None, None, Some(locked))?;
            Ok((ApplyOutcome::Edited(EditOutcome::UpdatedTrack(track)), Some(inverse)))
        }
        EditCommand::RippleDelete { clip } => {
            let inverse = edit::ripple_delete::execute(ctx, clip)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Removed(clip)), Some(inverse)))
        }
        EditCommand::ShiftClips { track, from, delta } => {
            let inverse = edit::shift_clips::execute(ctx, track, from, delta)?;
            Ok((ApplyOutcome::Edited(EditOutcome::ShiftedTrack(track)), Some(inverse)))
        }
        EditCommand::RippleInsert {
            track,
            media,
            source,
            at,
        } => {
            let (id, inverse) = edit::ripple_insert::execute(ctx, track, media, source, at)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Created(id)), Some(inverse)))
        }
    }
}
