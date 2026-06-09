//! Command dispatch: undoable timeline edits and session project commands.

mod dispatch;
mod edit;
mod project;

pub use dispatch::{ApplyOutcome, dispatch};

use cutlass_cache::FrameCache;
use cutlass_models::Project;

use crate::error::EngineError;

/// Session surface passed to every command handler.
pub struct ApplyContext<'a> {
    pub project: &'a mut Project,
    pub cache: &'a FrameCache,
    pub project_path: &'a mut Option<std::path::PathBuf>,
    pub history: &'a mut History,
}

/// A runtime edit action. Consuming `apply` runs the edit and returns its inverse.
pub trait EditAction: Send {
    fn apply(self: Box<Self>, ctx: &mut ApplyContext<'_>) -> Result<Box<dyn EditAction>, EngineError>;
}

/// Inverse-command undo/redo stacks.
#[derive(Default)]
pub struct History {
    undo: Vec<Box<dyn EditAction>>,
    redo: Vec<Box<dyn EditAction>>,
    limit: usize,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: limit.max(1),
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn record_do(&mut self, inverse: Box<dyn EditAction>) {
        if self.undo.len() >= self.limit {
            self.undo.remove(0);
        }
        self.undo.push(inverse);
        self.redo.clear();
    }

    pub fn pop_undo(&mut self) -> Option<Box<dyn EditAction>> {
        self.undo.pop()
    }

    pub fn push_redo(&mut self, inverse: Box<dyn EditAction>) {
        if self.redo.len() >= self.limit {
            self.redo.remove(0);
        }
        self.redo.push(inverse);
    }

    pub fn pop_redo(&mut self) -> Option<Box<dyn EditAction>> {
        self.redo.pop()
    }

    pub fn push_undo(&mut self, inverse: Box<dyn EditAction>) {
        if self.undo.len() >= self.limit {
            self.undo.remove(0);
        }
        self.undo.push(inverse);
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::edit::{add_clip, remove_media};
    use cutlass_cache::FrameCache;
    use cutlass_models::{MediaSource, Rational, RationalTime, TimeRange, TrackKind};

    fn setup() -> (tempfile::TempDir, Project, FrameCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = FrameCache::new(dir.path().join("cache"), 1024 * 1024).unwrap();
        let project = Project::new("test", Rational::FPS_24);
        (dir, project, cache)
    }

    fn test_ctx<'a>(
        project: &'a mut Project,
        cache: &'a FrameCache,
        project_path: &'a mut Option<std::path::PathBuf>,
        history: &'a mut History,
    ) -> ApplyContext<'a> {
        ApplyContext {
            project,
            cache,
            project_path,
            history,
        }
    }

    #[test]
    fn add_clip_inverse_oscillates() {
        let (_dir, mut project, cache) = setup();
        let media_id = project.add_media(MediaSource::new(
            "/tmp/x.mp4",
            1920,
            1080,
            Rational::FPS_24,
            240,
            true,
        ));
        let track = project.add_track(TrackKind::Video, "V1");

        let mut project_path = None;
        let mut history = History::new(32);
        let mut ctx = test_ctx(&mut project, &cache, &mut project_path, &mut history);

        let (id, inv1) = add_clip::execute(
            &mut ctx,
            track,
            media_id,
            TimeRange::at_rate(0, 48, Rational::FPS_24),
            RationalTime::new(0, Rational::FPS_24),
        )
        .unwrap();
        assert_eq!(ctx.project.timeline().clip_count(), 1);

        let inv2 = inv1.apply(&mut ctx).unwrap();
        assert_eq!(ctx.project.timeline().clip_count(), 0);

        let inv3 = inv2.apply(&mut ctx).unwrap();
        assert_eq!(ctx.project.timeline().clip_count(), 1);
        assert!(ctx.project.clip(id).is_some());

        let _ = inv3.apply(&mut ctx).unwrap();
        assert_eq!(ctx.project.timeline().clip_count(), 0);
    }

    #[test]
    fn remove_media_inverse_restores_snapshot() {
        let (_dir, mut project, cache) = setup();
        let media = MediaSource::new("/tmp/y.mp4", 1280, 720, Rational::FPS_24, 100, false);
        let id = project.add_media(media.clone());

        let mut project_path = None;
        let mut history = History::new(32);
        let mut ctx = test_ctx(&mut project, &cache, &mut project_path, &mut history);

        let inv1 = Box::new(remove_media::RemoveMediaAction { media: id });
        let inv2 = inv1.apply(&mut ctx).unwrap();
        assert_eq!(ctx.project.media_count(), 0);

        let inv3 = inv2.apply(&mut ctx).unwrap();
        assert_eq!(ctx.project.media_count(), 1);
        assert_eq!(ctx.project.media(id).unwrap(), &media);

        let _ = inv3.apply(&mut ctx).unwrap();
        assert_eq!(ctx.project.media_count(), 0);
    }
}
