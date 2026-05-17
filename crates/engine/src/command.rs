use std::path::PathBuf;

use decoder::Rational;

use crate::ids::{RequestId, SourceId};

/// Commands sent to the decode worker. **Owned** values only — no borrows across threads.
///
/// Interactive scrub uses a separate slot + signal on [`crate::Engine`](super::Engine); there is no `SeekScrub` variant.
#[derive(Debug)]
pub enum EngineCommand {
    /// Open one media path under `source_id`. MVP allows only one active decoder; see [`crate::Engine::open`](super::Engine::open).
    Open {
        source_id: SourceId,
        path: PathBuf,
        request_id: RequestId,
    },
    /// Presentation-accurate seek; echoed on [`crate::EngineEvent::Frame`](super::EngineEvent::Frame) / [`crate::EngineEvent::Eof`](super::EngineEvent::Eof) with `request_id: Some`.
    SeekExact {
        source_id: SourceId,
        target: Rational,
        request_id: RequestId,
    },
    /// Decode one more frame after the current read cursor.
    NextFrame {
        source_id: SourceId,
        request_id: RequestId,
    },
    /// Drop `source_id` from the worker map (no `request_id`; reply is [`crate::EngineEvent::Closed`](super::EngineEvent::Closed)).
    Close {
        source_id: SourceId,
    },
}
