use decoder::Rational;

use crate::ids::{ClipId, MediaSourceId};
use crate::model::{Clip, Track};
use crate::time::{add, sub};

/// Active clip on a track at a timeline time, with mapped source media time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveClip {
    pub clip_id: ClipId,
    pub source_id: MediaSourceId,
    pub media_time: Rational,
}

pub fn active_clip_on_track(track: &Track, timeline_time: Rational) -> Option<ActiveClip> {
    let idx = binary_search_clip(track.clips.as_slice(), timeline_time)?;
    let clip = &track.clips[idx];
    let offset = sub(timeline_time, clip.timeline_position)?;
    let media_time = add(clip.source_in, offset)?;
    Some(ActiveClip {
        clip_id: clip.id,
        source_id: clip.source_id,
        media_time,
    })
}

/// Returns index of clip whose half-open timeline range contains `t`, if any.
fn binary_search_clip(clips: &[Clip], t: Rational) -> Option<usize> {
    if clips.is_empty() {
        return None;
    }
    let mut lo = 0usize;
    let mut hi = clips.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let clip = &clips[mid];
        if clip.contains_timeline_time(t) {
            return Some(mid);
        }
        let Some(end) = clip.timeline_end() else {
            return None;
        };
        if t.ge(clip.timeline_position) {
            // t is at or after this clip's start; search right if past end.
            if t.ge(end) {
                lo = mid + 1;
            } else {
                return None;
            }
        } else {
            hi = mid;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ClipId, MediaSourceId, TrackId};
    use crate::model::{Clip, Track, TrackKind};
    use decoder::Rational;

    fn sample_clip(id: u64, pos: i64, dur: i64) -> Clip {
        Clip {
            id: ClipId(id),
            source_id: MediaSourceId(1),
            source_in: Rational::new_raw(0, 1),
            source_out: Rational::new_raw(dur, 1),
            timeline_position: Rational::new_raw(pos, 1),
        }
    }

    #[test]
    fn maps_time_inside_clip() {
        let track = Track {
            id: TrackId(1),
            kind: TrackKind::Video,
            clips: vec![
                sample_clip(1, 0, 5),
                sample_clip(2, 5, 3),
            ],
            muted: false,
            locked: false,
        };
        let active = active_clip_on_track(&track, Rational::new_raw(6, 1)).expect("hit");
        assert_eq!(active.clip_id, ClipId(2));
        assert_eq!(active.media_time.reduced(), Rational::new_raw(1, 1));
    }

    #[test]
    fn gap_returns_none() {
        let track = Track {
            id: TrackId(1),
            kind: TrackKind::Video,
            clips: vec![sample_clip(1, 0, 2)],
            muted: false,
            locked: false,
        };
        assert!(active_clip_on_track(&track, Rational::new_raw(5, 1)).is_none());
    }
}
