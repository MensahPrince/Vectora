//! Undoable timeline edits as inverse [`EditAction`](crate::action::EditAction) pairs.

pub mod add_clip;
pub mod insert_clip;
pub mod insert_media;
pub mod legacy;
pub mod remove_clip;
pub mod remove_media;
