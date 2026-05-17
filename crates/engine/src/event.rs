use decoder::{DecodedVideoFrame, SourceInfo};

use crate::error::EngineError;
use crate::ids::{RequestId, SourceId};

/// Events from the decode worker. **Owned** payloads — consumers take [`DecodedVideoFrame`] by value.
///
/// ## `request_id`
/// - [`Opened`](EngineEvent::Opened): always `Some` / required field on the variant.
/// - [`Frame`](EngineEvent::Frame) / [`Eof`](EngineEvent::Eof): `Some` for [`crate::EngineCommand`](super::EngineCommand) driven work; `None` for [`crate::Engine::seek_scrub`](super::Engine::seek_scrub).
/// - [`Error`](EngineEvent::Error): mirrors the originating command when applicable.
#[derive(Debug)]
pub enum EngineEvent {
    /// Decoder opened successfully after [`crate::EngineCommand::Open`](super::EngineCommand::Open).
    Opened {
        source_id: SourceId,
        info: SourceInfo,
        request_id: RequestId,
    },
    /// One decoded video frame.
    Frame {
        source_id: SourceId,
        frame: DecodedVideoFrame,
        /// `None` for scrub-driven frames.
        request_id: Option<RequestId>,
    },
    /// Demuxer/decoder drained for this operation (not an error).
    Eof {
        source_id: SourceId,
        request_id: Option<RequestId>,
    },
    /// Recoverable or fatal problem (includes [`crate::EngineError::Decoder`](super::EngineError::Decoder)).
    Error {
        source_id: Option<SourceId>,
        error: EngineError,
        request_id: Option<RequestId>,
    },
    /// Source removed from the worker map after [`crate::EngineCommand::Close`](super::EngineCommand::Close).
    Closed {
        source_id: SourceId,
    },
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;

    use decoder::{CpuFrame, DecodedVideoFrame, FrameData, Plane, PixelFormat, Rational};

    use super::EngineEvent;
    use crate::ids::{RequestId, SourceId};

    fn dummy_frame() -> DecodedVideoFrame {
        DecodedVideoFrame {
            width: 2,
            height: 2,
            pts: Rational::new_raw(0, 1),
            timebase: Rational::new_raw(1, 30_000),
            data: FrameData::Cpu(CpuFrame {
                format: PixelFormat::Rgba8,
                planes: vec![Plane {
                    data: vec![0u8; 16],
                    stride: 8,
                }],
            }),
        }
    }

    #[test]
    fn frame_event_round_trip_unbounded_channel_is_send() {
        let (tx, rx) = unbounded();
        let sid = SourceId(1);
        let req = RequestId(2);
        let frame = dummy_frame();
        tx.send(EngineEvent::Frame {
            source_id: sid,
            frame: frame.clone(),
            request_id: Some(req),
        })
        .expect("send");

        match rx.recv().expect("recv") {
            EngineEvent::Frame {
                source_id,
                frame: got,
                request_id,
            } => {
                assert_eq!(source_id, sid);
                assert_eq!(request_id, Some(req));
                assert_eq!(got, frame);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
