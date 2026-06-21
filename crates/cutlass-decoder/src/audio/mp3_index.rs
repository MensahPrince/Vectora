//! Lazily-built, frame-exact seek index for MP3 streams.
//!
//! MP3 has no container sample table: FFmpeg's default seek estimates a byte
//! offset from the average bitrate, lands on whatever frame sync follows, and
//! assigns it a *re-estimated* PTS. For VBR files (or any file the estimate
//! misjudges) that anchor is wrong, so a seek that should hit sample `S` lands
//! tens of milliseconds off — audible as a flam when scrubbing or as A/V drift
//! against a frame-exact video track.
//!
//! The fix is the same shape the video path uses ([`crate::video`]'s keyframe
//! index): demux the file **once, without decoding**, and record every audio
//! packet's `(pts, byte_offset)`. Each MP3 frame is independently decodable, so
//! a byte seek to a recorded frame boundary is exact — the reader then anchors
//! the position from the *index* (a known sample count), never the decoder's
//! re-estimated PTS. Built lazily on the first hard seek (one I/O-bound demux
//! pass) and cached for the reader's life.
//!
//! All lookups are integer-only in the stream's `time_base` units. For MP3 the
//! demuxer sets `time_base = 1/sample_rate`, so a `pts` *is* a sample index;
//! the reader keeps the conversion general against `time_base` anyway.

use std::path::Path;

use ffmpeg_next::format;
use ffmpeg_next::media::Type;
use ffmpeg_next::packet::Packet;
use ffmpeg_next::{Error as FfmpegError, codec};
use tracing::debug;

use crate::error::DecodeError;
use crate::video::ensure_ffmpeg_init;

/// One indexed MP3 frame: its presentation timestamp (stream `time_base`
/// ticks) and the file byte offset of its first byte (a frame-sync boundary,
/// the exact target for `AVSEEK_FLAG_BYTE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp3Entry {
    pub pts: i64,
    pub byte: i64,
}

/// Frame-exact MP3 seek map: ascending, de-duplicated `(pts, byte)` entries.
#[derive(Debug, Clone)]
pub struct Mp3SeekIndex {
    entries: Vec<Mp3Entry>,
}

impl Mp3SeekIndex {
    /// Demux `path` once (no decode) and record every audio packet's PTS and
    /// byte offset. Errors when the best audio stream is not MP3 or carries no
    /// byte-positioned frames — callers fall back to the PTS seek path.
    pub fn build(path: &Path) -> Result<Self, DecodeError> {
        ensure_ffmpeg_init()?;

        let path_str = path
            .to_str()
            .ok_or_else(|| DecodeError::unsupported("path is not valid UTF-8"))?;
        let mut input = format::input(path_str).map_err(DecodeError::Open)?;

        let (stream_index, codec_id) = {
            let stream = input
                .streams()
                .best(Type::Audio)
                .ok_or_else(|| DecodeError::unsupported("no audio stream found"))?;
            (stream.index(), stream.parameters().id())
        };
        if codec_id != codec::Id::MP3 {
            return Err(DecodeError::unsupported("stream is not MP3"));
        }

        let mut entries: Vec<Mp3Entry> = Vec::new();
        // Fallback PTS for the rare frame the demuxer hands over without one:
        // accumulate frame durations from the last known anchor.
        let mut running_pts: i64 = 0;
        loop {
            // Fresh packet each iteration: `Packet::read` (av_read_frame) does
            // not unref the previous buffer, and `Packet` only unrefs on drop —
            // reusing one packet across the whole file leaks every packet's
            // payload (GBs on a long source).
            let mut packet = Packet::empty();
            match packet.read(&mut input) {
                Ok(()) => {
                    if packet.stream() != stream_index {
                        continue;
                    }
                    let pts = packet.pts().unwrap_or(running_pts);
                    let byte = packet.position();
                    if byte >= 0 {
                        entries.push(Mp3Entry {
                            pts,
                            byte: byte as i64,
                        });
                    }
                    let dur = packet.duration().max(0);
                    running_pts = pts + dur;
                }
                Err(FfmpegError::Eof) => break,
                Err(e) => return Err(DecodeError::Io(e)),
            }
        }

        entries.sort_by_key(|e| e.pts);
        entries.dedup_by_key(|e| e.pts);
        if entries.is_empty() {
            return Err(DecodeError::unsupported(
                "no byte-positioned MP3 frames found",
            ));
        }

        debug!(frames = entries.len(), "built mp3 seek index");
        Ok(Self { entries })
    }

    /// The frame with the largest `pts <= target_pts`, or the first frame when
    /// the target precedes the index — so a too-early seek still lands at the
    /// head rather than failing. The `<=` predicate means a target exactly on a
    /// frame boundary selects that frame, not the one before it.
    pub fn entry_at_or_before(&self, target_pts: i64) -> Mp3Entry {
        match self.entries.partition_point(|e| e.pts <= target_pts) {
            0 => self.entries[0],
            n => self.entries[n - 1],
        }
    }
}

#[cfg(test)]
impl Mp3SeekIndex {
    /// Synthetic index for unit tests (sorted + de-duplicated by pts).
    pub(crate) fn from_entries(mut entries: Vec<Mp3Entry>) -> Self {
        entries.sort_by_key(|e| e.pts);
        entries.dedup_by_key(|e| e.pts);
        Self { entries }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> Mp3SeekIndex {
        // 1152-sample frames at a 1/rate time base ⇒ pts == sample.
        Mp3SeekIndex::from_entries(vec![
            Mp3Entry { pts: 0, byte: 100 },
            Mp3Entry {
                pts: 1152,
                byte: 518,
            },
            Mp3Entry {
                pts: 2304,
                byte: 936,
            },
            Mp3Entry {
                pts: 3456,
                byte: 1354,
            },
        ])
    }

    #[test]
    fn at_or_before_selects_containing_frame() {
        let idx = index();
        // Inside the second frame: take the second frame's boundary.
        assert_eq!(
            idx.entry_at_or_before(1500),
            Mp3Entry {
                pts: 1152,
                byte: 518
            }
        );
    }

    #[test]
    fn exact_boundary_selects_itself() {
        let idx = index();
        assert_eq!(
            idx.entry_at_or_before(2304),
            Mp3Entry {
                pts: 2304,
                byte: 936
            }
        );
    }

    #[test]
    fn before_first_returns_head() {
        let idx = index();
        assert_eq!(idx.entry_at_or_before(-100), Mp3Entry { pts: 0, byte: 100 });
    }

    #[test]
    fn past_last_returns_tail() {
        let idx = index();
        assert_eq!(
            idx.entry_at_or_before(1_000_000),
            Mp3Entry {
                pts: 3456,
                byte: 1354
            }
        );
    }

    #[test]
    fn from_entries_sorts_and_dedups() {
        // Out-of-order with a duplicate pts: sorted, the head wins lookups and
        // the tail is still reachable (so the dedup kept distinct frames).
        let idx = Mp3SeekIndex::from_entries(vec![
            Mp3Entry {
                pts: 2304,
                byte: 936,
            },
            Mp3Entry { pts: 0, byte: 100 },
            Mp3Entry { pts: 0, byte: 100 },
        ]);
        assert_eq!(idx.entry_at_or_before(0).byte, 100);
        assert_eq!(idx.entry_at_or_before(5000).byte, 936);
    }
}
