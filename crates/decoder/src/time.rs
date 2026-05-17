//! Rational timestamps: **source of truth** for media seconds in the public API.
//! Prefer [`Rational::as_f64`] only for logging or UI labels — comparisons use [`Rational::ge`]
//! or cross-multiplication, not floats.

use std::fmt;

/// A rational number `num / den` representing **time in seconds** (or a factor thereof from stream ticks).
///
/// - **Invariant:** `den != 0` (enforced by constructors).
/// - **PTS / duration:** See [`Rational::from_stream_ticks`] for converting decoder ticks × FFmpeg `time_base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    /// Numerator (signed). For time in seconds, this is the “top” of the second fraction after normalization or construction.
    pub num: i64,
    /// Denominator (strictly positive in valid values).
    pub den: u32,
}

impl Rational {
    /// Returns `None` if `den == 0`.
    pub const fn new(num: i64, den: u32) -> Option<Self> {
        if den == 0 {
            return None;
        }
        Some(Self {num, den})
    }

    /// # Panics
    /// Panics if `den == 0` (use in tests or after validation).
    pub const fn new_raw(num: i64, den: u32) -> Self {
        assert!(den != 0, "Rational denominator must be non-zero");
        Self {num, den}
    }

    /// Display-only conversion; **not** authoritative for comparisons on long timelines.
    pub fn as_f64(self) -> f64 {
        self.num as f64 / f64::from(self.den)
    }

    /// Converts FFmpeg `pts` ticks and stream `time_base` into **seconds** as a rational.
    /// Returns `None` if the product does not fit or `time_base` is invalid.
    pub fn from_stream_ticks(pts: i64, time_base: ffmpeg_next::Rational) -> Option<Self> {
        let tb_num = i64::from(time_base.numerator());
        let tb_den = i64::from(time_base.denominator());
        if tb_den == 0 {
            return None;
        }
        let n = (pts as i128).checked_mul(tb_num as i128)?;
        let d = tb_den as i128;
        reduce_i128(n, d)
    }

    /// Container-reported stream duration: `ticks × time_base` → seconds, when duration is valid.
    /// Returns `None` for unknown / unset duration (`AV_NOPTS_VALUE` or non-positive ticks).
    pub fn from_duration_ticks(ticks: i64, time_base: ffmpeg_next::Rational) -> Option<Self> {
        const NOPTS: i64 = 0x8000_0000_0000_0000u64 as i64;
        if ticks <= 0 || ticks == NOPTS {
            return None;
        }
        Self::from_stream_ticks(ticks, time_base)
    }

    /// Whether `self` ≥ `other` in the ordering on ℚ (both rationals well-formed).
    pub fn ge(self, other: Rational) -> bool {
        (self.num as i128) * (other.den as i128) >= (other.num as i128) * (self.den as i128)
    }

    /// Reduces `num`/`den` by GCD when the result fits in `i64`/`u32`; otherwise returns `self`.
    pub fn reduced(self) -> Self {
        reduce_i128(self.num as i128, self.den as i128).unwrap_or(self)
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

fn reduce_i128(num: i128, den: i128) -> Option<Rational> {
    if den == 0 {
        return None;
    }
    let sign = den.signum();
    let mut n = num;
    let mut d = den * sign;
    if d == 0 {
        return None;
    }
    let g = gcd_u128(n.unsigned_abs(), d.unsigned_abs()) as i128;
    n /= g;
    d /= g;
    if n > i64::MAX as i128 || n < i64::MIN as i128 || d > u32::MAX as i128 {
        return None;
    }
    Some(Rational {
        num: n as i64,
        den: d as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::Rational;
    use ffmpeg_next::Rational as Fr;

    #[test]
    fn new_rejects_zero_denominator() {
        assert!(Rational::new(1, 0).is_none());
    }

    #[test]
    fn ge_reflexive() {
        let r = Rational::new_raw(3, 7);
        assert!(r.ge(r));
    }

    #[test]
    fn from_duration_rejects_nopts() {
        let tb = Fr::new(1, 30_000);
        const NOPTS: i64 = 0x8000_0000_0000_0000u64 as i64;
        assert!(Rational::from_duration_ticks(NOPTS, tb).is_none());
        assert!(Rational::from_duration_ticks(0, tb).is_none());
        assert!(Rational::from_duration_ticks(-1, tb).is_none());
    }

    #[test]
    fn from_stream_ticks_identity_small() {
        let tb = Fr::new(1, 30_000);
        let r = Rational::from_stream_ticks(60_000, tb).expect("ticks");
        assert_eq!(r.reduced(), Rational::new_raw(2, 1));
    }

    #[test]
    fn reduces_60_30() {
        let r = Rational::new_raw(60, 30).reduced();
        assert_eq!(r, Rational::new_raw(2, 1));
    }

    #[test]
    fn ge_cross_multiply() {
        let a = Rational::new_raw(5, 2); // 2.5
        let b = Rational::new_raw(9, 4); // 2.25
        assert!(a.ge(b));
        assert!(!b.ge(a));
    }

    #[test]
    fn as_f64_display_only() {
        let r = Rational::new_raw(1, 3);
        assert!((r.as_f64() - (1.0 / 3.0)).abs() < 1e-12);
    }
}
