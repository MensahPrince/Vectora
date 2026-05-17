use crate::error::TimelineError;
use crate::model::Project;

/// Reversible edit applied to [`Project`].
pub trait Command: Send {
    fn apply(&mut self, project: &mut Project) -> Result<(), TimelineError>;
    fn undo(&mut self, project: &mut Project);
    fn label(&self) -> &str;
}
