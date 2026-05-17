use std::fmt;
use uuid::Uuid;

/// Stable identity for a project file (cross-machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ProjectId(pub Uuid);

/// Media source within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MediaSourceId(pub u64);

/// Timeline track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TrackId(pub u64);

/// Clip on a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ClipId(pub u64);

impl fmt::Display for MediaSourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "source:{}", self.0)
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "track:{}", self.0)
    }
}

impl fmt::Display for ClipId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "clip:{}", self.0)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct IdAllocator {
    next_source: u64,
    next_track: u64,
    next_clip: u64,
}

impl IdAllocator {
    pub fn alloc_source(&mut self) -> MediaSourceId {
        let id = MediaSourceId(self.next_source);
        self.next_source = self.next_source.saturating_add(1);
        id
    }

    pub fn alloc_track(&mut self) -> TrackId {
        let id = TrackId(self.next_track);
        self.next_track = self.next_track.saturating_add(1);
        id
    }

    pub fn alloc_clip(&mut self) -> ClipId {
        let id = ClipId(self.next_clip);
        self.next_clip = self.next_clip.saturating_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn allocator_issues_unique_ids() {
        let mut a = IdAllocator::default();
        let s0 = a.alloc_source();
        let s1 = a.alloc_source();
        let t0 = a.alloc_track();
        let c0 = a.alloc_clip();
        assert_ne!(s0, s1);
        assert_eq!(t0, TrackId(0));
        assert_eq!(c0, ClipId(0));
        assert_eq!(s0, MediaSourceId(0));
        assert_eq!(s1, MediaSourceId(1));
    }

    #[test]
    fn ids_hash_and_eq() {
        let mut set = HashSet::new();
        set.insert(ClipId(1));
        assert!(set.contains(&ClipId(1)));
        assert!(!set.contains(&ClipId(2)));
    }

    #[test]
    fn display_formats() {
        assert_eq!(MediaSourceId(3).to_string(), "source:3");
        assert_eq!(TrackId(4).to_string(), "track:4");
        assert_eq!(ClipId(5).to_string(), "clip:5");
    }
}
