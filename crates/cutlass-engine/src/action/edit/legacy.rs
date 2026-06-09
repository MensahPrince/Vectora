//! Bridge for edits not yet migrated to inverse actions.

use cutlass_commands::{EditCommand, EditOutcome};
use cutlass_models::Project;

use crate::action::{ApplyContext, EditAction};
use crate::error::EngineError;

pub struct RestoreProject {
    project: Project,
}

impl EditAction for RestoreProject {
    fn apply(self: Box<Self>, ctx: &mut ApplyContext<'_>) -> Result<Box<dyn EditAction>, EngineError> {
        let current = std::mem::replace(ctx.project, self.project);
        Ok(Box::new(RestoreProject { project: current }))
    }
}

pub fn apply(
    ctx: &mut ApplyContext<'_>,
    command: EditCommand,
) -> Result<(EditOutcome, Box<dyn EditAction>), EngineError> {
    let snapshot = ctx.project.clone();
    let outcome = match command {
        EditCommand::AddGenerated {
            track,
            generator,
            timeline,
        } => {
            let id = ctx.project.add_generated(track, generator, timeline)?;
            EditOutcome::Created(id)
        }
        EditCommand::SplitClip { clip, at } => {
            let id = ctx.project.split_clip(clip, at)?;
            EditOutcome::Created(id)
        }
        EditCommand::TrimClip { clip, timeline } => {
            ctx.project.trim_clip(clip, timeline)?;
            EditOutcome::Updated(clip)
        }
        EditCommand::MoveClip {
            clip,
            to_track,
            start,
        } => {
            ctx.project.move_clip(clip, to_track, start)?;
            EditOutcome::Updated(clip)
        }
        EditCommand::RippleDelete { clip } => {
            ctx.project.ripple_delete(clip)?;
            EditOutcome::Removed(clip)
        }
        EditCommand::AddClip { .. } | EditCommand::RemoveClip { .. } => {
            unreachable!("handled by inverse actions")
        }
    };
    Ok((outcome, Box::new(RestoreProject { project: snapshot })))
}
