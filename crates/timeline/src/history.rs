use crate::command::Command;
use crate::error::TimelineError;
use crate::model::Project;

const DEFAULT_MAX_DEPTH: usize = 100;

/// Bounded undo/redo stacks for edit commands.
pub struct History {
    undo_stack: Vec<Box<dyn Command>>,
    redo_stack: Vec<Box<dyn Command>>,
    max_depth: usize,
}

impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History")
            .field("undo_depth", &self.undo_stack.len())
            .field("redo_depth", &self.redo_stack.len())
            .field("max_depth", &self.max_depth)
            .finish()
    }
}

impl Default for History {
    fn default() -> Self {
        Self::with_max_depth(DEFAULT_MAX_DEPTH)
    }
}

impl History {
    pub fn with_max_depth(max_depth: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_depth: max_depth.max(1),
        }
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    pub fn push_executed(&mut self, cmd: Box<dyn Command>) {
        self.redo_stack.clear();
        self.undo_stack.push(cmd);
        while self.undo_stack.len() > self.max_depth {
            self.undo_stack.remove(0);
        }
    }

    pub fn pop_undo(&mut self) -> Option<Box<dyn Command>> {
        self.undo_stack.pop()
    }

    pub fn push_redo(&mut self, cmd: Box<dyn Command>) {
        self.redo_stack.push(cmd);
    }

    pub fn pop_redo(&mut self) -> Option<Box<dyn Command>> {
        self.redo_stack.pop()
    }
}

impl Project {
    /// Apply a command, optionally recording it in undo history.
    pub fn apply(
        &mut self,
        mut cmd: Box<dyn Command>,
        record_history: bool,
    ) -> Result<(), TimelineError> {
        cmd.apply(self)?;
        if record_history {
            self.history.push_executed(cmd);
        }
        Ok(())
    }

    pub fn undo(&mut self) -> Result<bool, TimelineError> {
        let Some(mut cmd) = self.history.pop_undo() else {
            return Ok(false);
        };
        cmd.undo(self);
        self.history.push_redo(cmd);
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool, TimelineError> {
        let Some(mut cmd) = self.history.pop_redo() else {
            return Ok(false);
        };
        cmd.apply(self)?;
        self.history.push_executed(cmd);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::model::Project;

    struct BumpSchema;

    impl Command for BumpSchema {
        fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError> {
            project.schema_version += 1;
            Ok(())
        }

        fn undo(&mut self, project: &mut Project) {
            project.schema_version -= 1;
        }

        fn label(&self) -> &str {
            "bump schema"
        }
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut p = Project::new();
        let v0 = p.schema_version;
        p.apply(Box::new(BumpSchema), true).unwrap();
        assert_eq!(p.schema_version, v0 + 1);
        p.undo().unwrap();
        assert_eq!(p.schema_version, v0);
        p.redo().unwrap();
        assert_eq!(p.schema_version, v0 + 1);
    }
}
