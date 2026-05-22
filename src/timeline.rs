use crate::{Rational, RationalTime, Sequence};
use slint::Model;

pub fn sequence_duration(sequence: Sequence) -> RationalTime {
    let sequence_rate = sequence.fps.clone();
    let mut max_end = 0;

    for track_index in 0..sequence.tracks.row_count() {
        let Some(track) = sequence.tracks.row_data(track_index) else {
            continue;
        };

        for clip_index in 0..track.clips.row_count() {
            let Some(clip) = track.clips.row_data(clip_index) else {
                continue;
            };

            let start = to_rate_value(&clip.timeline_start, &sequence_rate);
            let duration = to_rate_value(&clip.source_range.duration, &sequence_rate);
            max_end = max_end.max(start + duration);
        }
    }

    RationalTime {
        value: max_end,
        rate: sequence_rate,
    }
}

fn to_rate_value(time: &RationalTime, target_rate: &Rational) -> i32 {
    let numerator = i64::from(time.value)
        * i64::from(time.rate.den)
        * i64::from(target_rate.num);
    let denominator = i64::from(time.rate.num) * i64::from(target_rate.den);

    ceil_div(numerator, denominator).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn ceil_div(numerator: i64, denominator: i64) -> i64 {
    debug_assert!(denominator > 0);

    if numerator >= 0 {
        (numerator + denominator - 1) / denominator
    } else {
        numerator / denominator
    }
}
