use cutlass_commands::{Command, EditCommand, EditOutcome, ProjectCommand};

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
            Ok((ApplyOutcome::Imported { media }, Some(inverse)))
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
    }
}

fn dispatch_edit(
    edit: EditCommand,
    ctx: &mut ApplyContext<'_>,
) -> Result<(ApplyOutcome, Option<Box<dyn EditAction>>), EngineError> {
    match edit {
        EditCommand::AddClip {
            track,
            media,
            source,
            start,
        } => {
            let (id, inverse) = edit::add_clip::execute(ctx, track, media, source, start)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Created(id)), Some(inverse)))
        }
        EditCommand::RemoveClip { clip } => {
            let inverse = Box::new(RemoveClipAction { clip }).apply(ctx)?;
            Ok((ApplyOutcome::Edited(EditOutcome::Removed(clip)), Some(inverse)))
        }
        other => edit::legacy::apply(ctx, other)
            .map(|(outcome, inverse)| (ApplyOutcome::Edited(outcome), Some(inverse))),
    }
}
