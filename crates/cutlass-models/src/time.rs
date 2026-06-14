//! Time primitives aligned with OpenTimelineIO: exact [`Rational`] rates and
//! [`RationalTime`] positions/durations.
//!
//! A [`RationalTime`] is `value` ticks at `rate` ticks-per-second. NTSC rates
//! (23.976 = 24000/1001) stay exact. [`TimeRange`] is half-open
//! `[start, start + duration)` with both endpoints carrying the same rate.

use crate::error::ModelError;
use serde::{Deserialize, Serialize};

/// An exact frame rate as `num/den` frames per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    pub const FPS_24: Rational = Rational::new(24, 1);
    pub const FPS_23_976: Rational = Rational::new(24000, 1001);
    pub const FPS_25: Rational = Rational::new(25, 1);
    pub const FPS_30: Rational = Rational::new(30, 1);
    pub const FPS_29_97: Rational = Rational::new(30000, 1001);
    pub const FPS_50: Rational = Rational::new(50, 1);
    pub const FPS_60: Rational = Rational::new(60, 1);
    pub const FPS_59_94: Rational = Rational::new(60000, 1001);

    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// Approximate frames-per-second as a float (for display/UI only).
    pub fn as_f64(self) -> f64 {
        if self.den == 0 {
            return 0.0;
        }
        f64::from(self.num) / f64::from(self.den)
    }

    /// Seconds-per-frame as a float.
    pub fn seconds_per_frame(self) -> f64 {
        if self.num == 0 {
            return 0.0;
        }
        f64::from(self.den) / f64::from(self.num)
    }

    pub fn is_valid(self) -> bool {
        self.num > 0 && self.den > 0
    }
}

/// A time position or duration as integer ticks at an exact rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RationalTime {
    pub value: i64,
    pub rate: Rational,
}

impl RationalTime {
    pub const fn new(value: i64, rate: Rational) -> Self {
        Self { value, rate }
    }

    pub fn zero(rate: Rational) -> Self {
        Self::new(0, rate)
    }
}

/// Half-open range `[start, start + duration)`.
///
/// `start` and `duration` must share the same `rate`; use [`TimeRange::at_rate`]
/// when constructing from tick counts at one rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: RationalTime,
    pub duration: RationalTime,
}

impl TimeRange {
    pub fn new(start: RationalTime, duration: RationalTime) -> Result<Self, ModelError> {
        check_same_rate(start.rate, duration.rate)?;
        Ok(Self { start, duration })
    }

    /// Build a range from tick counts at a single rate.
    pub fn at_rate(start: i64, duration: i64, rate: Rational) -> Self {
        Self {
            start: RationalTime::new(start, rate),
            duration: RationalTime::new(duration, rate),
        }
    }

    /// Exclusive end tick (`start + duration`) at the range's rate.
    pub fn end_tick(self) -> i64 {
        self.start.value + self.duration.value
    }

    /// Exclusive end as a [`RationalTime`].
    pub fn end(self) -> Result<RationalTime, ModelError> {
        time_add(&self.start, &self.duration)
    }

    pub fn is_empty(self) -> bool {
        self.duration.value <= 0
    }

    /// True if `t` lies within `[start, end)`.
    pub fn contains(self, t: RationalTime) -> Result<bool, ModelError> {
        check_same_rate(self.start.rate, t.rate)?;
        Ok(t.value >= self.start.value && t.value < self.end_tick())
    }

    /// True if the two ranges share at least one tick.
    pub fn overlaps(self, other: Self) -> Result<bool, ModelError> {
        check_same_rate(self.start.rate, other.start.rate)?;
        if self.is_empty() || other.is_empty() {
            return Ok(false);
        }
        Ok(self.start.value < other.end_tick() && other.start.value < self.end_tick())
    }

    /// The overlapping region, if any.
    pub fn intersection(self, other: Self) -> Result<Option<Self>, ModelError> {
        if !self.overlaps(other)? {
            return Ok(None);
        }
        let start = self.start.value.max(other.start.value);
        let end = self.end_tick().min(other.end_tick());
        Ok(Some(Self::at_rate(start, end - start, self.start.rate)))
    }
}

/// Structural equality on the rate pair (no gcd reduction).
pub fn rate_eq(a: Rational, b: Rational) -> bool {
    a.num == b.num && a.den == b.den
}

/// Errors when `actual` does not match `expected`.
pub fn check_same_rate(actual: Rational, expected: Rational) -> Result<(), ModelError> {
    if rate_eq(actual, expected) {
        Ok(())
    } else {
        Err(ModelError::RateMismatch {
            expected,
            got: actual,
        })
    }
}

/// Same-rate addition with checked overflow.
pub fn time_add(a: &RationalTime, b: &RationalTime) -> Result<RationalTime, ModelError> {
    check_same_rate(a.rate, b.rate)?;
    let value = a
        .value
        .checked_add(b.value)
        .ok_or(ModelError::TimeOverflow)?;
    Ok(RationalTime::new(value, a.rate))
}

/// Same-rate subtraction with checked overflow.
pub fn time_sub(a: &RationalTime, b: &RationalTime) -> Result<RationalTime, ModelError> {
    check_same_rate(a.rate, b.rate)?;
    let value = a
        .value
        .checked_sub(b.value)
        .ok_or(ModelError::TimeOverflow)?;
    Ok(RationalTime::new(value, a.rate))
}

/// Resample `time` to `to`, rounding to the nearest tick.
pub fn resample(time: RationalTime, to: Rational) -> RationalTime {
    if !time.rate.is_valid() || !to.is_valid() {
        return RationalTime::zero(to);
    }
    if rate_eq(time.rate, to) {
        return time;
    }
    let numer = i128::from(time.value) * i128::from(to.num) * i128::from(time.rate.den);
    let denom = i128::from(to.den) * i128::from(time.rate.num);
    if denom == 0 {
        return RationalTime::zero(to);
    }
    let half = denom.abs() / 2;
    let magnitude = (numer.abs() + half) / denom.abs();
    let q = if (numer >= 0) == (denom >= 0) {
        magnitude
    } else {
        -magnitude
    };
    RationalTime::new(q as i64, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    const R24: Rational = Rational::FPS_24;
    const R30: Rational = Rational::FPS_30;

    fn rt(value: i64, rate: Rational) -> RationalTime {
        RationalTime::new(value, rate)
    }

    fn tr(start: i64, duration: i64, rate: Rational) -> TimeRange {
        TimeRange::at_rate(start, duration, rate)
    }

    // --- Rational ---------------------------------------------------------

    #[test]
    fn rational_fps_constants_are_canonical() {
        assert_eq!(Rational::FPS_23_976, Rational::new(24000, 1001));
        assert_eq!(Rational::FPS_29_97, Rational::new(30000, 1001));
        assert_eq!(Rational::FPS_59_94, Rational::new(60000, 1001));
        assert_eq!(Rational::FPS_24, Rational::new(24, 1));
        assert_eq!(Rational::FPS_25, Rational::new(25, 1));
        assert_eq!(Rational::FPS_30, Rational::new(30, 1));
        assert_eq!(Rational::FPS_50, Rational::new(50, 1));
        assert_eq!(Rational::FPS_60, Rational::new(60, 1));
    }

    #[test]
    fn rational_as_f64_approximates_ntsc() {
        assert!((Rational::FPS_23_976.as_f64() - 23.976).abs() < 0.001);
        assert!((Rational::FPS_29_97.as_f64() - 29.97).abs() < 0.001);
        assert!((Rational::FPS_59_94.as_f64() - 59.94).abs() < 0.001);
        assert!((Rational::FPS_24.as_f64() - 24.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rational_seconds_per_frame() {
        assert!((Rational::FPS_24.seconds_per_frame() - 1.0 / 24.0).abs() < f64::EPSILON);
        assert!((Rational::FPS_30.seconds_per_frame() - 1.0 / 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rational_as_f64_handles_degenerate_denominator() {
        assert_eq!(Rational::new(24, 0).as_f64(), 0.0);
    }

    #[test]
    fn rational_seconds_per_frame_handles_zero_numerator() {
        assert_eq!(Rational::new(0, 1).seconds_per_frame(), 0.0);
    }

    #[test]
    fn rational_is_valid() {
        assert!(Rational::new(24, 1).is_valid());
        assert!(Rational::new(24000, 1001).is_valid());
        assert!(!Rational::new(0, 1).is_valid());
        assert!(!Rational::new(24, 0).is_valid());
        assert!(!Rational::new(-24, 1).is_valid());
        assert!(!Rational::new(24, -1).is_valid());
    }

    // --- RationalTime -----------------------------------------------------

    #[test]
    fn rational_time_zero() {
        let z = RationalTime::zero(R30);
        assert_eq!(z.value, 0);
        assert_eq!(z.rate, R30);
    }

    // --- rate_eq / check_same_rate ----------------------------------------

    #[test]
    fn rate_eq_is_structural_not_reduced() {
        assert!(rate_eq(R30, R30));
        // 30/1 and 60/2 are mathematically equal but stored differently.
        assert!(!rate_eq(Rational::new(30, 1), Rational::new(60, 2)));
    }

    #[test]
    fn check_same_rate_ok_and_err() {
        assert!(check_same_rate(R24, R24).is_ok());
        // `check_same_rate(actual, expected)` — first arg is what was found.
        let err = check_same_rate(R24, R30).unwrap_err();
        assert_eq!(
            err,
            ModelError::RateMismatch {
                expected: R30,
                got: R24,
            }
        );
    }

    // --- time_add / time_sub ----------------------------------------------

    #[test]
    fn time_add_same_rate() {
        let a = rt(30, R30);
        let b = rt(45, R30);
        let sum = time_add(&a, &b).unwrap();
        assert_eq!(sum.value, 75);
        assert_eq!(sum.rate, R30);
    }

    #[test]
    fn time_add_zero_is_identity() {
        let t = rt(100, R24);
        assert_eq!(time_add(&t, &RationalTime::zero(R24)).unwrap(), t);
    }

    #[test]
    fn time_add_rate_mismatch() {
        assert!(matches!(
            time_add(&rt(1, R30), &rt(1, R24)).unwrap_err(),
            ModelError::RateMismatch { .. }
        ));
    }

    #[test]
    fn time_add_overflow() {
        assert_eq!(
            time_add(&rt(i64::MAX, R24), &rt(1, R24)).unwrap_err(),
            ModelError::TimeOverflow
        );
    }

    #[test]
    fn time_sub_same_rate_and_negative_result() {
        let a = rt(30, R30);
        let b = rt(45, R30);
        assert_eq!(time_sub(&a, &b).unwrap().value, -15);
    }

    #[test]
    fn time_sub_rate_mismatch() {
        assert!(matches!(
            time_sub(&rt(10, R30), &rt(1, R24)).unwrap_err(),
            ModelError::RateMismatch { .. }
        ));
    }

    #[test]
    fn time_sub_overflow() {
        assert_eq!(
            time_sub(&rt(i64::MIN, R24), &rt(1, R24)).unwrap_err(),
            ModelError::TimeOverflow
        );
    }

    // --- TimeRange construction -------------------------------------------

    #[test]
    fn time_range_new_requires_matching_rates() {
        let start = rt(0, R24);
        let duration = rt(100, R24);
        assert_eq!(TimeRange::new(start, duration).unwrap().start, start);

        let bad_duration = rt(100, R30);
        assert_eq!(
            TimeRange::new(start, bad_duration).unwrap_err(),
            ModelError::RateMismatch {
                expected: R30,
                got: R24,
            }
        );
    }

    #[test]
    fn time_range_at_rate_builds_matching_endpoints() {
        let range = tr(10, 5, R24);
        assert_eq!(range.start, rt(10, R24));
        assert_eq!(range.duration, rt(5, R24));
    }

    #[test]
    fn time_range_end_matches_end_tick() {
        let range = tr(10, 5, R24);
        assert_eq!(range.end_tick(), 15);
        assert_eq!(range.end().unwrap(), rt(15, R24));
    }

    #[test]
    fn time_range_is_empty() {
        assert!(tr(0, 0, R24).is_empty());
        assert!(tr(10, -1, R24).is_empty());
        assert!(!tr(0, 1, R24).is_empty());
    }

    // --- TimeRange::contains ----------------------------------------------

    #[test]
    fn range_contains_half_open_boundaries() {
        let r = tr(10, 5, R24); // [10, 15)
        assert!(r.contains(rt(10, R24)).unwrap());
        assert!(r.contains(rt(14, R24)).unwrap());
        assert!(!r.contains(rt(15, R24)).unwrap());
        assert!(!r.contains(rt(9, R24)).unwrap());
    }

    #[test]
    fn range_contains_empty_is_always_false() {
        let empty = tr(10, 0, R24);
        assert!(!empty.contains(rt(10, R24)).unwrap());
    }

    #[test]
    fn range_contains_rate_mismatch() {
        let r = tr(0, 10, R24);
        assert_eq!(
            r.contains(rt(5, R30)).unwrap_err(),
            ModelError::RateMismatch {
                expected: R30,
                got: R24,
            }
        );
    }

    // --- TimeRange::overlaps / intersection -------------------------------

    #[test]
    fn overlap_touching_ranges_do_not_overlap() {
        let a = tr(0, 10, R24);
        let b = tr(10, 5, R24);
        assert!(!a.overlaps(b).unwrap());
        assert!(!b.overlaps(a).unwrap());
        assert_eq!(a.intersection(b).unwrap(), None);
    }

    #[test]
    fn overlap_partial_and_nested() {
        let a = tr(0, 10, R24);
        let c = tr(5, 10, R24);
        assert!(a.overlaps(c).unwrap());
        assert_eq!(a.intersection(c).unwrap(), Some(tr(5, 5, R24)));

        let nested = tr(2, 3, R24);
        assert!(a.overlaps(nested).unwrap());
        assert_eq!(a.intersection(nested).unwrap(), Some(nested));

        let identical = tr(0, 10, R24);
        assert!(a.overlaps(identical).unwrap());
        assert_eq!(a.intersection(identical).unwrap(), Some(identical));
    }

    #[test]
    fn overlap_disjoint_ranges() {
        let a = tr(0, 10, R24);
        let d = tr(20, 5, R24);
        assert!(!a.overlaps(d).unwrap());
        assert_eq!(a.intersection(d).unwrap(), None);
    }

    #[test]
    fn overlap_single_tick_intersection() {
        let a = tr(0, 5, R24);
        let b = tr(4, 5, R24);
        assert!(a.overlaps(b).unwrap());
        assert_eq!(a.intersection(b).unwrap(), Some(tr(4, 1, R24)));
    }

    #[test]
    fn empty_range_never_overlaps() {
        let empty = tr(5, 0, R24);
        let other = tr(0, 100, R24);
        assert!(empty.is_empty());
        assert!(!empty.overlaps(other).unwrap());
        assert!(!other.overlaps(empty).unwrap());
        assert_eq!(empty.intersection(other).unwrap(), None);
    }

    #[test]
    fn overlap_rate_mismatch() {
        let a = tr(0, 10, R24);
        let b = tr(0, 10, R30);
        assert_eq!(
            a.overlaps(b).unwrap_err(),
            ModelError::RateMismatch {
                expected: R30,
                got: R24,
            }
        );
    }

    // --- resample ---------------------------------------------------------

    #[test]
    fn resample_identity_and_common_conversions() {
        let t = rt(50, R24);
        assert_eq!(resample(t, R24), t);

        assert_eq!(resample(rt(100, R30), R24).value, 80);
        assert_eq!(resample(rt(80, R24), R30).value, 100);

        // 1001 frames @ 24000/1001 (~41.75s) -> 1002 frames @ 24.
        assert_eq!(resample(rt(1001, Rational::FPS_23_976), R24).value, 1002);
    }

    #[test]
    fn resample_zero_stays_zero() {
        assert_eq!(resample(rt(0, R30), R24).value, 0);
    }

    #[test]
    fn resample_rounds_to_nearest() {
        // 1 frame @ 30fps = 0.0333s -> 0.8 frames @ 24fps -> rounds to 1.
        assert_eq!(resample(rt(1, R30), R24).value, 1);
        // 2 frames @ 30fps = 0.0666s -> 1.6 frames @ 24fps -> rounds to 2.
        assert_eq!(resample(rt(2, R30), R24).value, 2);
    }

    #[test]
    fn resample_negative_value() {
        // -100 frames @ 30fps -> -80 frames @ 24fps (nearest).
        assert_eq!(resample(rt(-100, R30), R24).value, -80);
    }

    #[test]
    fn resample_invalid_rates_yield_zero_at_target() {
        let invalid = Rational::new(0, 1);
        assert_eq!(resample(rt(100, invalid), R24), rt(0, R24));
        assert_eq!(resample(rt(100, R24), invalid), rt(0, invalid));
    }

    #[test]
    fn resample_preserves_exact_wall_clock_duration() {
        // 60 seconds at 30fps = 1800 ticks -> exactly 1440 ticks at 24fps.
        let one_minute_30 = rt(1_800, R30);
        let at_24 = resample(one_minute_30, R24);
        assert_eq!(at_24.value, 1_440);
        assert_eq!(at_24.rate, R24);

        // Round-trip back to 30fps lands on the original tick count.
        assert_eq!(resample(at_24, R30).value, 1_800);
    }
}
