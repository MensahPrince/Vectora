//! Rational time arithmetic for the timeline model (no floats).

use std::cmp::Ordering;

use decoder::Rational;

pub fn cmp(a: Rational, b: Rational) -> Ordering {
    let lhs = (a.num as i128) * (b.den as i128);
    let rhs = (b.num as i128) * (a.den as i128);
    lhs.cmp(&rhs)
}

pub fn lt(a: Rational, b: Rational) -> bool {
    cmp(a, b) == Ordering::Less
}

pub fn le(a: Rational, b: Rational) -> bool {
    cmp(a, b) != Ordering::Greater
}

pub fn gt(a: Rational, b: Rational) -> bool {
    cmp(a, b) == Ordering::Greater
}

/// `a + b` in reduced form when the result fits.
pub fn add(a: Rational, b: Rational) -> Option<Rational> {
    let n = (a.num as i128)
        .checked_mul(b.den as i128)?
        .checked_add((b.num as i128).checked_mul(a.den as i128)?)?;
    let d = (a.den as i128).checked_mul(b.den as i128)?;
    reduce_i128(n, d)
}

/// `a - b` in reduced form when the result fits.
pub fn sub(a: Rational, b: Rational) -> Option<Rational> {
    let n = (a.num as i128)
        .checked_mul(b.den as i128)?
        .checked_sub((b.num as i128).checked_mul(a.den as i128)?)?;
    let d = (a.den as i128).checked_mul(b.den as i128)?;
    reduce_i128(n, d)
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
    if n > i64::MAX as i128 || n < i64::MIN as i128 || d > u32::MAX as i128 || d <= 0 {
        return None;
    }
    Some(Rational {
        num: n as i64,
        den: d as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use decoder::Rational;

    #[test]
    fn add_sub_round_trip() {
        let a = Rational::new_raw(5, 2);
        let b = Rational::new_raw(1, 4);
        let sum = add(a, b).expect("add");
        assert_eq!(sum.reduced(), Rational::new_raw(11, 4));
        let diff = sub(sum, b).expect("sub");
        assert_eq!(diff.reduced(), a.reduced());
    }

    #[test]
    fn cmp_matches_ge() {
        let a = Rational::new_raw(3, 1);
        let b = Rational::new_raw(5, 2);
        assert!(a.ge(b));
        assert_eq!(cmp(a, b), Ordering::Greater);
    }

    #[test]
    fn cmp_reflexive() {
        let r = Rational::new_raw(7, 13);
        assert_eq!(cmp(r, r), Ordering::Equal);
    }

    #[test]
    fn lt_le_gt_consistent() {
        let a = Rational::new_raw(1, 3);
        let b = Rational::new_raw(1, 2);
        assert!(lt(a, b));
        assert!(le(a, b));
        assert!(!gt(a, b));
        assert!(gt(b, a));
        assert!(!lt(b, a));
    }

    #[test]
    fn add_commutative_when_reduced() {
        let a = Rational::new_raw(1, 6);
        let b = Rational::new_raw(1, 4);
        assert_eq!(
            add(a, b).unwrap().reduced(),
            add(b, a).unwrap().reduced()
        );
    }

    #[test]
    fn sub_self_is_zero() {
        let a = Rational::new_raw(22, 7);
        let z = sub(a, a).unwrap();
        assert_eq!(z.reduced(), Rational::new_raw(0, 1));
    }

    #[test]
    fn add_large_denominators() {
        let a = Rational::new_raw(1, 30_000);
        let b = Rational::new_raw(1001, 30_000);
        let s = add(a, b).expect("add");
        assert_eq!(
            s.reduced(),
            Rational::new_raw(1002, 30_000).reduced()
        );
    }

    #[test]
    fn add_negative_rationals() {
        let a = Rational::new_raw(-3, 4);
        let b = Rational::new_raw(1, 2);
        let s = add(a, b).expect("add");
        assert_eq!(s.reduced(), Rational::new_raw(-1, 4));
    }

    #[test]
    fn overflow_add_returns_none() {
        let huge = Rational::new_raw(i64::MAX, 1);
        assert!(add(huge, huge).is_none());
    }
}
