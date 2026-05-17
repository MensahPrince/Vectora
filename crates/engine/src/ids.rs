use std::fmt;

/// Opaque handle for one media source (decoder instance in the worker map).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub u64);

/// Correlates a command with matching [`crate::EngineEvent`](super::EngineEvent)s (`Opened`, `Frame`, `Eof`, `Error`).
///
/// Scrub-driven frames use `request_id: None` on events; see [`crate::Engine`](super::Engine) scrub API (later phase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{RequestId, SourceId};
    use std::collections::HashSet;

    #[test]
    fn source_id_eq_hash() {
        let a = SourceId(1);
        let b = SourceId(1);
        let c = SourceId(2);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }

    #[test]
    fn request_id_eq_hash() {
        let a = RequestId(10);
        let b = RequestId(10);
        assert_eq!(a, b);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn display_ids() {
        assert_eq!(SourceId(42).to_string(), "42");
        assert_eq!(RequestId(7).to_string(), "7");
    }
}
