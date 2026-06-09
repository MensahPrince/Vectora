//! Keyframe index for fast, predictable seeking.
//!
//! [`KeyframeIndex::build`] demuxes a file once (no decode) and records the
//! presentation timestamp of every keyframe on the best video stream. The index
//! is the *map*, not the *driver*: it answers "given a target tick, which
//! keyframe do I seek to, and where does that GOP end?" — leaving the actual
//! seek + decode walk to the runtime reader that owns the demuxer.
//!
//! All hot-path queries are integer-only, in stream `time_base` ticks. The
//! [`Duration`] helpers exist for display/UI and tests and must never sit on
//! the path that decides which keyframe a seek lands on — that is exactly where
//! float drift turns an exact target into a wrong keyframe.

use std::path::Path;
use std::time::Duration;

use ffmpeg_next::format;
use ffmpeg_next::media::Type;
use ffmpeg_next::packet::Packet;
use ffmpeg_next::{Error as FfmpegError, Rational, Rescale, rescale};
use tracing::{debug, warn};

use crate::error::DecodeError;
use crate::video::decoder::ensure_ffmpeg_init;

/// A group-of-pictures span, in stream `time_base` ticks.
///
/// `start` is the keyframe to seek to. `end` is the *next* keyframe's PTS — the
/// exclusive upper bound of this GOP — or `None` when this is the final GOP.
/// The reader uses [`Gop::contains`] to decide whether a new target still lives
/// in the GOP it is already decoding (skip the seek) or crossed a boundary
/// (must seek + flush).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gop {
    /// Keyframe PTS at the head of this GOP — the seek entry point.
    pub start: i64,
    /// PTS of the next GOP's keyframe (exclusive bound), or `None` if last.
    pub end: Option<i64>,
}

impl Gop {
    /// True when `ticks` falls within `[start, end)`.
    #[inline]
    pub fn contains(&self, ticks: i64) -> bool {
        ticks >= self.start && self.end.is_none_or(|e| ticks < e)
    }
}

#[derive(Debug, Clone)]
pub struct KeyframeIndex {
    time_base: Rational,
    /// Ascending, de-duplicated keyframe PTS values (stream `time_base` ticks).
    keyframes: Vec<i64>,
}

impl KeyframeIndex {
    /// Demux `path` once and collect every keyframe's presentation timestamp.
    ///
    /// This does not decode any frames — it only inspects packet flags, so the
    /// cost is dominated by I/O and is suitable to run at import time.
    pub fn build(path: &Path) -> Result<Self, DecodeError> {
        ensure_ffmpeg_init()?;

        let path_str = path
            .to_str()
            .ok_or_else(|| DecodeError::unsupported("path is not valid UTF-8"))?;

        let mut input = format::input(path_str).map_err(DecodeError::Open)?;

        let (stream_index, time_base) = {
            let stream = input
                .streams()
                .best(Type::Video)
                .ok_or_else(|| DecodeError::unsupported("no video stream found"))?;
            (stream.index(), stream.time_base())
        };

        let mut keyframes = Vec::new();
        let mut dts_fallbacks = 0usize;
        let mut packet = Packet::empty();
        loop {
            match packet.read(&mut input) {
                Ok(()) => {
                    if packet.stream() == stream_index && packet.is_key() {
                        // Prefer PTS: it is what the decoder presents and what
                        // our at-or-before lookup is later compared against. DTS
                        // is a fallback only; if it ever fires, the index tick
                        // may not match the presented PTS and at-or-before can
                        // be a GOP off.
                        match packet.pts() {
                            Some(pts) => keyframes.push(pts),
                            None => {
                                if let Some(dts) = packet.dts() {
                                    dts_fallbacks += 1;
                                    keyframes.push(dts);
                                }
                            }
                        }
                    }
                }
                Err(FfmpegError::Eof) => break,
                Err(e) => return Err(DecodeError::Io(e)),
            }
        }

        keyframes.sort_unstable();
        keyframes.dedup();

        if keyframes.is_empty() {
            return Err(DecodeError::unsupported("no keyframes found in stream"));
        }
        if dts_fallbacks > 0 {
            warn!(dts_fallbacks, "some keyframes lacked PTS; fell back to DTS");
        }

        debug!(
            count = keyframes.len(),
            tb_num = time_base.numerator(),
            tb_den = time_base.denominator(),
            "built keyframe index"
        );

        Ok(Self {
            time_base,
            keyframes,
        })
    }

    // ---- metadata -----------------------------------------------------------

    /// The stream `time_base` these ticks are expressed in.
    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    /// Keyframe presentation timestamps in ascending stream `time_base` ticks.
    pub fn keyframe_ticks(&self) -> &[i64] {
        &self.keyframes
    }

    /// Number of indexed keyframes (always >= 1 after a successful build).
    pub fn len(&self) -> usize {
        self.keyframes.len()
    }

    /// True when no keyframes were found. (Build refuses to return such an
    /// index, so this is only ever false on a live index — kept for the
    /// `clippy::len_without_is_empty` lint and symmetry.)
    pub fn is_empty(&self) -> bool {
        self.keyframes.is_empty()
    }

    // ---- hot path: integer ticks only --------------------------------------

    /// Slot of the latest keyframe at or before `target_ticks`.
    ///
    /// `partition_point` counts entries `<= target`; the last of those is the
    /// at-or-before keyframe. Because the predicate is `<=`, a target landing
    /// *exactly* on a keyframe selects that keyframe — not the one before it.
    #[inline]
    fn slot_at_or_before(&self, target_ticks: i64) -> Option<usize> {
        match self.keyframes.partition_point(|&k| k <= target_ticks) {
            0 => None,
            n => Some(n - 1),
        }
    }

    /// Latest keyframe PTS at or before `target_ticks` (stream tb), if any.
    ///
    /// This is *the link* between the index and a seek: the returned tick is the
    /// only valid demux entry point for decoding up to `target_ticks`.
    pub fn keyframe_at_or_before_ticks(&self, target_ticks: i64) -> Option<i64> {
        self.slot_at_or_before(target_ticks)
            .map(|i| self.keyframes[i])
    }

    /// The GOP containing `target_ticks`: its entry keyframe and next boundary.
    ///
    /// This is the primitive the runtime reader leans on. One binary search
    /// yields both halves of the seek decision:
    ///
    /// ```ignore
    /// let gop = index.gop_containing(target)?;
    /// let need_seek = decode_head > target          // scrubbed backward
    ///     || !gop.contains(decode_head);            // crossed a GOP boundary
    /// if need_seek {
    ///     let us = index.ticks_to_av_time_base(gop.start);
    ///     input.seek(us, ..=us)?;
    ///     decoder.flush();                          // avcodec_flush_buffers
    /// }
    /// // then walk forward, dropping frames until pts >= target
    /// ```
    pub fn gop_containing(&self, target_ticks: i64) -> Option<Gop> {
        let i = self.slot_at_or_before(target_ticks)?;
        Some(Gop {
            start: self.keyframes[i],
            end: self.keyframes.get(i + 1).copied(),
        })
    }

    /// PTS that starts the GOP *after* the one containing `ticks`, if any.
    pub fn next_keyframe_ticks(&self, ticks: i64) -> Option<i64> {
        let i = self.slot_at_or_before(ticks)?;
        self.keyframes.get(i + 1).copied()
    }

    /// Rescale a stream-tb tick into `AV_TIME_BASE` microseconds — the unit the
    /// high-level [`format::context::Input::seek`] expects, because it passes
    /// `stream_index = -1`.
    ///
    /// Uses FFmpeg's own integer `av_rescale_q` rounding (via the [`Rescale`]
    /// trait), so the value round-trips back to the same packet FFmpeg would
    /// pick. No f64 anywhere on the seek path.
    pub fn ticks_to_av_time_base(&self, ticks: i64) -> i64 {
        ticks.rescale(self.time_base, rescale::TIME_BASE)
    }

    /// The `AV_TIME_BASE` µs seek target for the keyframe at or before
    /// `target_ticks`. Feed straight into `input.seek(us, ..=us)`.
    pub fn seek_us_at_or_before(&self, target_ticks: i64) -> Option<i64> {
        self.keyframe_at_or_before_ticks(target_ticks)
            .map(|kf| self.ticks_to_av_time_base(kf))
    }

    // ---- display / UI only: NOT for seek decisions -------------------------

    /// Keyframe presentation times as wall-clock [`Duration`]s, ascending.
    /// For UI (ruler ticks, thumbnail strips) — never to choose a seek target.
    pub fn keyframe_times(&self) -> impl Iterator<Item = Duration> + '_ {
        let tb = self.time_base;
        self.keyframes
            .iter()
            .map(move |&ticks| ticks_to_duration(tb, ticks))
    }

    /// Convert stream-tb ticks into a wall-clock [`Duration`]. Display only.
    pub fn ticks_to_duration(&self, ticks: i64) -> Duration {
        ticks_to_duration(self.time_base, ticks)
    }

    /// Convert a wall-clock [`Duration`] into stream-tb ticks. Display only.
    pub fn duration_to_ticks(&self, target: Duration) -> i64 {
        duration_to_ticks(self.time_base, target)
    }

    /// Latest keyframe time at or before `target`, if any. Display only — do
    /// not route a real seek target through this (the `Duration` hop loses the
    /// exactness your `RationalTime` carries).
    pub fn keyframe_at_or_before(&self, target: Duration) -> Option<Duration> {
        self.keyframe_at_or_before_ticks(self.duration_to_ticks(target))
            .map(|ticks| self.ticks_to_duration(ticks))
    }
}

#[cfg(test)]
impl KeyframeIndex {
    /// Synthetic index for unit tests (sorted + de-duplicated).
    pub(crate) fn from_keyframes(time_base: Rational, mut keyframes: Vec<i64>) -> Self {
        keyframes.sort_unstable();
        keyframes.dedup();
        Self {
            time_base,
            keyframes,
        }
    }
}

/// ticks → [`Duration`] via i128 integer math (no float drift). Display only.
pub fn ticks_to_duration(time_base: Rational, ticks: i64) -> Duration {
    let num = i128::from(time_base.numerator());
    let den = i128::from(time_base.denominator());
    if num <= 0 || den <= 0 || ticks < 0 {
        return Duration::ZERO;
    }
    // nanos = ticks * (num/den) * 1e9, evaluated exactly in i128.
    let nanos = (i128::from(ticks) * num * 1_000_000_000) / den;
    Duration::from_nanos(nanos.clamp(0, i128::from(u64::MAX)) as u64)
}

/// [`Duration`] → ticks via i128 integer math (truncates toward zero). Display
/// only.
pub fn duration_to_ticks(time_base: Rational, target: Duration) -> i64 {
    let num = i128::from(time_base.numerator());
    let den = i128::from(time_base.denominator());
    if num <= 0 || den <= 0 {
        return 0;
    }
    // ticks = nanos * den / (num * 1e9).
    let nanos = target.as_nanos() as i128;
    let ticks = (nanos * den) / (num * 1_000_000_000);
    ticks.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tb(num: i32, den: i32) -> Rational {
        Rational::new(num, den)
    }

    fn synthetic_index(kfs: &[i64]) -> KeyframeIndex {
        KeyframeIndex::from_keyframes(tb(1, 24), kfs.to_vec())
    }

    #[test]
    fn gop_contains_respects_exclusive_end() {
        let gop = Gop {
            start: 100,
            end: Some(200),
        };
        assert!(gop.contains(100));
        assert!(gop.contains(199));
        assert!(!gop.contains(200));
        assert!(!gop.contains(99));
    }

    #[test]
    fn gop_contains_open_ended_tail() {
        let gop = Gop {
            start: 500,
            end: None,
        };
        assert!(gop.contains(500));
        assert!(gop.contains(10_000));
    }

    #[test]
    fn ticks_to_duration_zero_tick_is_zero_time() {
        let tb = tb(1, 24);
        assert_eq!(ticks_to_duration(tb, 0), Duration::ZERO);
        assert_eq!(ticks_to_duration(tb, 24), Duration::from_secs(1));
        assert_eq!(ticks_to_duration(tb, 12), Duration::from_nanos(500_000_000));
    }

    #[test]
    fn ticks_to_duration_negative_ticks_are_zero() {
        assert_eq!(ticks_to_duration(tb(1, 24), -5), Duration::ZERO);
    }

    #[test]
    fn ticks_to_duration_invalid_time_base_is_zero() {
        assert_eq!(ticks_to_duration(tb(0, 24), 100), Duration::ZERO);
        assert_eq!(ticks_to_duration(tb(1, 0), 100), Duration::ZERO);
    }

    #[test]
    fn duration_to_ticks_roundtrip_at_frame_boundaries() {
        let tb = tb(1, 24);
        for secs in [0_u64, 1, 2, 5] {
            let d = Duration::from_secs(secs);
            let ticks = duration_to_ticks(tb, d);
            assert_eq!(ticks, (secs * 24) as i64);
            assert_eq!(ticks_to_duration(tb, ticks), d);
        }
    }

    #[test]
    fn duration_to_ticks_truncates_sub_frame_offsets() {
        let tb = tb(1, 24);
        let d = Duration::from_millis(100); // 2.4 frames at 24fps
        assert_eq!(duration_to_ticks(tb, d), 2);
    }

    #[test]
    fn duration_to_ticks_invalid_time_base_is_zero() {
        assert_eq!(duration_to_ticks(tb(0, 1), Duration::from_secs(1)), 0);
    }

    #[test]
    fn keyframe_lookup_before_first_returns_none() {
        let index = synthetic_index(&[100, 200, 300]);
        assert_eq!(index.keyframe_at_or_before_ticks(99), None);
        assert_eq!(index.gop_containing(50), None);
        assert_eq!(index.seek_us_at_or_before(50), None);
    }

    #[test]
    fn keyframe_lookup_exact_and_between() {
        let index = synthetic_index(&[0, 100, 200]);
        assert_eq!(index.keyframe_at_or_before_ticks(0), Some(0));
        assert_eq!(index.keyframe_at_or_before_ticks(150), Some(100));
        assert_eq!(index.keyframe_at_or_before_ticks(200), Some(200));
        assert_eq!(index.keyframe_at_or_before_ticks(201), Some(200));
    }

    #[test]
    fn next_keyframe_ticks_skips_current_gop() {
        let index = synthetic_index(&[0, 100, 250]);
        assert_eq!(index.next_keyframe_ticks(50), Some(100));
        assert_eq!(index.next_keyframe_ticks(100), Some(250));
        assert_eq!(index.next_keyframe_ticks(300), None);
    }

    #[test]
    fn gop_containing_maps_to_half_open_interval() {
        let index = synthetic_index(&[0, 100, 200]);
        let mid = index.gop_containing(50).expect("gop");
        assert_eq!(mid.start, 0);
        assert_eq!(mid.end, Some(100));
        assert!(mid.contains(50));
        assert!(!mid.contains(100));
    }

    #[test]
    fn seek_us_at_or_before_uses_av_time_base() {
        let index = KeyframeIndex::from_keyframes(tb(1, 1_000_000), vec![2_000_000]);
        let us = index.seek_us_at_or_before(2_500_000).expect("seek");
        assert_eq!(us, 2_000_000);
    }

    #[test]
    fn from_keyframes_sorts_and_dedups() {
        let index = KeyframeIndex::from_keyframes(tb(1, 24), vec![300, 100, 200, 200]);
        assert_eq!(index.keyframe_ticks(), &[100, 200, 300]);
    }

    #[test]
    fn is_empty_reflects_keyframe_count() {
        let index = KeyframeIndex::from_keyframes(tb(1, 24), vec![0]);
        assert!(!index.is_empty());
        assert_eq!(index.len(), 1);
    }

    fn any_video_asset() -> Option<PathBuf> {
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        let preferred = assets.join("6137050-hd_1920_1080_24fps.mp4");
        if preferred.exists() {
            return Some(preferred);
        }
        std::fs::read_dir(assets)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "mp4"))
    }

    #[test]
    fn build_collects_keyframes() {
        let Some(path) = any_video_asset() else {
            return;
        };
        let index = KeyframeIndex::build(&path).expect("build");
        assert!(!index.is_empty());
        assert!(index.keyframe_ticks().windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn keyframe_at_or_before_is_monotonic() {
        let Some(path) = any_video_asset() else {
            return;
        };
        let index = KeyframeIndex::build(&path).expect("build");
        let target = Duration::from_millis(500);
        let entry = index.keyframe_at_or_before(target).expect("entry");
        assert!(entry <= target);
    }

    #[test]
    fn first_keyframe_is_at_or_before_zero() {
        let Some(path) = any_video_asset() else {
            return;
        };
        let index = KeyframeIndex::build(&path).expect("build");
        let first = index.keyframe_at_or_before(Duration::ZERO).expect("first");
        assert_eq!(first, index.ticks_to_duration(index.keyframe_ticks()[0]));
    }

    #[test]
    fn exact_keyframe_tick_selects_itself_not_prior() {
        let Some(path) = any_video_asset() else {
            return;
        };
        let index = KeyframeIndex::build(&path).expect("build");
        let kfs = index.keyframe_ticks();
        if kfs.len() < 2 {
            return;
        }
        // Querying a keyframe's own tick must return that keyframe.
        let exact = kfs[1];
        assert_eq!(index.keyframe_at_or_before_ticks(exact), Some(exact));
        // One tick before it falls back to the previous keyframe.
        assert_eq!(index.keyframe_at_or_before_ticks(exact - 1), Some(kfs[0]));
    }

    #[test]
    fn gop_spans_consecutive_keyframes() {
        let Some(path) = any_video_asset() else {
            return;
        };
        let index = KeyframeIndex::build(&path).expect("build");
        let kfs = index.keyframe_ticks();
        if kfs.len() < 2 {
            return;
        }
        // A tick midway through the first GOP resolves to [kf0, kf1).
        let mid = kfs[0] + (kfs[1] - kfs[0]) / 2;
        let gop = index.gop_containing(mid).expect("gop");
        assert_eq!(gop.start, kfs[0]);
        assert_eq!(gop.end, Some(kfs[1]));
        assert!(gop.contains(mid));
        assert!(!gop.contains(kfs[1])); // boundary is exclusive

        // The final GOP has no upper bound.
        let last = *kfs.last().unwrap();
        let tail = index.gop_containing(last).expect("tail gop");
        assert_eq!(tail.start, last);
        assert_eq!(tail.end, None);
        assert!(tail.contains(last + 1));
    }

    #[test]
    fn av_time_base_rescale_is_monotonic() {
        let Some(path) = any_video_asset() else {
            return;
        };
        let index = KeyframeIndex::build(&path).expect("build");
        let us: Vec<i64> = index
            .keyframe_ticks()
            .iter()
            .map(|&t| index.ticks_to_av_time_base(t))
            .collect();
        assert!(us.windows(2).all(|w| w[0] <= w[1]));
    }
}
