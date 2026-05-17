//! Undo / redo stack behavior.

mod support;

use timeline::{AddSource, Project};

#[test]
fn undo_empty_returns_false() {
    let mut p = Project::new();
    assert!(!p.undo().unwrap());
}

#[test]
fn redo_empty_returns_false() {
    let mut p = Project::new();
    assert!(!p.redo().unwrap());
}

#[test]
fn new_edit_clears_redo_stack() {
    let mut p = Project::new();
    p.apply(Box::new(AddSource::new("/a.mp4")), true).unwrap();
    p.undo().unwrap();
    assert_eq!(p.history.redo_depth(), 1);
    p.apply(Box::new(AddSource::new("/b.mp4")), true).unwrap();
    assert_eq!(p.history.redo_depth(), 0);
}

#[test]
fn undo_depth_tracks_applied_commands() {
    let mut p = Project::new();
    assert_eq!(p.history.undo_depth(), 0);
    p.apply(Box::new(AddSource::new("/a.mp4")), true).unwrap();
    p.apply(Box::new(AddSource::new("/b.mp4")), true).unwrap();
    assert_eq!(p.history.undo_depth(), 2);
    p.undo().unwrap();
    assert_eq!(p.history.undo_depth(), 1);
}

#[test]
fn history_max_depth_drops_oldest_undo() {
    let mut p = Project::new();
    for i in 0..101 {
        p.apply(
            Box::new(AddSource::new(format!("/src{i}.mp4"))),
            true,
        )
        .unwrap();
    }
    assert_eq!(p.sources.len(), 101);
    assert_eq!(p.history.undo_depth(), 100);
    for _ in 0..100 {
        p.undo().unwrap();
    }
    assert!(!p.undo().unwrap());
    assert_eq!(p.sources.len(), 1);
}

#[test]
fn apply_without_history_leaves_undo_unchanged() {
    let mut p = Project::new();
    p.apply(Box::new(AddSource::new("/a.mp4")), false).unwrap();
    assert_eq!(p.history.undo_depth(), 0);
    assert_eq!(p.sources.len(), 1);
}

#[test]
fn redo_after_multiple_undo() {
    let mut p = Project::new();
    p.apply(Box::new(AddSource::new("/a.mp4")), true).unwrap();
    p.apply(Box::new(AddSource::new("/b.mp4")), true).unwrap();
    p.apply(Box::new(AddSource::new("/c.mp4")), true).unwrap();
    p.undo().unwrap();
    p.undo().unwrap();
    assert_eq!(p.sources.len(), 1);
    p.redo().unwrap();
    assert_eq!(p.sources.len(), 2);
}

#[test]
fn chained_undo_redo_undo() {
    let mut p = Project::new();
    p.apply(Box::new(AddSource::new("/a.mp4")), true).unwrap();
    p.apply(Box::new(AddSource::new("/b.mp4")), true).unwrap();
    p.undo().unwrap();
    p.redo().unwrap();
    p.undo().unwrap();
    p.undo().unwrap();
    assert!(p.sources.is_empty());
}
