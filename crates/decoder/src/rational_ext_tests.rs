//! Additional [`Rational`](crate::Rational) coverage kept separate so `time.rs` stays readable.

use crate::Rational;
use ffmpeg_next::Rational as Fr;

macro_rules! ge_case {
    ($name:ident, ($an:expr, $ad:expr), ($bn:expr, $bd:expr), $want:expr) => {
        #[test]
        fn $name() {
            let a = Rational::new_raw($an, $ad);
            let b = Rational::new_raw($bn, $bd);
            assert_eq!(a.ge(b), $want, "cmp {:?} >= {:?}", a, b);
        }
    };
}

ge_case!(ge_1_2_ge_1_3, (1, 2), (1, 3), true);
ge_case!(ge_1_3_not_ge_1_2, (1, 3), (1, 2), false);
ge_case!(ge_equal_fractions_2_4_vs_1_2, (2, 4), (1, 2), true);
ge_case!(ge_symmetric_equal_1_2_vs_2_4, (1, 2), (2, 4), true);
ge_case!(ge_three_seconds_gt_two, (3, 1), (2, 1), true);
ge_case!(ge_two_not_ge_three, (2, 1), (3, 1), false);
ge_case!(ge_same_den_greater_num, (5, 7), (4, 7), true);
ge_case!(ge_same_den_less_num, (4, 7), (5, 7), false);
ge_case!(ge_negative_ge_more_negative, (-1, 2), (-3, 4), true);
ge_case!(ge_more_negative_not_ge_less, (-3, 4), (-1, 2), false);
ge_case!(ge_positive_ge_negative, (1, 100), (-1, 1), true);
ge_case!(ge_negative_not_ge_positive, (-1, 100), (1, 1), false);
ge_case!(ge_zero_ge_negative, (0, 1), (-1, 1), true);
ge_case!(ge_zero_not_ge_small_positive, (0, 1), (1, 10_000), false);
ge_case!(ge_large_coords, (9_000_000_000_i64, 2), (4_000_000_000_i64, 1), true);
ge_case!(ge_resolution_wide_denominators, (1, 90000), (1, 90001), true);
ge_case!(ge_resolution_wide_denominators_rev, (1, 90001), (1, 90000), false);
ge_case!(ge_one_vs_almost_one, (1_000_001, 1_000_000), (1, 1), true);
ge_case!(ge_almost_one_vs_one, (999_999, 1_000_000), (1, 1), false);
ge_case!(ge_max_i64_over_two, (i64::MAX, 2), (i64::MAX - 1, 2), true);
ge_case!(ge_min_i64_less_than_next, (i64::MIN, 1), (i64::MIN + 1, 1), false);

#[test]
fn ge_transitivity_sample() {
    let a = Rational::new_raw(10, 3);
    let b = Rational::new_raw(3, 1);
    let c = Rational::new_raw(14, 5);
    assert!(a.ge(b));
    assert!(b.ge(c));
    assert!(a.ge(c));
}

#[test]
fn new_some_vs_none_zero_den() {
    assert!(Rational::new(0, 1).is_some());
    assert!(Rational::new(-7, 99).is_some());
    assert!(Rational::new(1, 0).is_none());
    assert!(Rational::new(0, 0).is_none());
}

#[test]
fn display_formats_slash() {
    assert_eq!(Rational::new_raw(-3, 8).to_string(), "-3/8");
    assert_eq!(Rational::new_raw(0, 1).to_string(), "0/1");
}

#[test]
fn partial_eq_compares_raw_components_not_reduced_math() {
    assert_ne!(Rational::new_raw(4, 4), Rational::new_raw(1, 1));
}

#[test]
fn partial_eq_same_components_match() {
    assert_eq!(Rational::new_raw(1, 2), Rational::new_raw(1, 2));
    assert_ne!(Rational::new_raw(1, 2), Rational::new_raw(2, 3));
}

#[test]
fn reduced_simple_fraction() {
    assert_eq!(
        Rational::new_raw(14, 21).reduced(),
        Rational::new_raw(2, 3)
    );
}

#[test]
fn reduced_negative_numerator() {
    assert_eq!(
        Rational::new_raw(-9, 12).reduced(),
        Rational::new_raw(-3, 4)
    );
}

#[test]
fn reduced_already_coprime() {
    let r = Rational::new_raw(17, 100);
    assert_eq!(r.reduced(), r);
}

#[test]
fn reduced_identity_one() {
    assert_eq!(Rational::new_raw(1, 1).reduced(), Rational::new_raw(1, 1));
}

#[test]
fn reduced_powers_of_two() {
    assert_eq!(
        Rational::new_raw(1024, 2048).reduced(),
        Rational::new_raw(1, 2)
    );
}

#[test]
fn from_stream_ticks_tb_den_zero_returns_none() {
    let tb = Fr::new(1, 0);
    assert!(Rational::from_stream_ticks(100, tb).is_none());
}

#[test]
fn from_stream_ticks_tb_num_zero_yields_zero_seconds() {
    let tb = Fr::new(0, 1);
    let r = Rational::from_stream_ticks(12345, tb).expect("valid");
    assert_eq!(r, Rational::new_raw(0, 1));
}

#[test]
fn from_stream_ticks_one_one_identity() {
    let tb = Fr::new(1, 1);
    assert_eq!(
        Rational::from_stream_ticks(42, tb).expect("ticks"),
        Rational::new_raw(42, 1)
    );
}

#[test]
fn from_stream_ticks_respects_timebase_ratio() {
    let tb = Fr::new(1, 60_000);
    let r = Rational::from_stream_ticks(120_000, tb).expect("ticks");
    assert_eq!(r.reduced(), Rational::new_raw(2, 1));
}

#[test]
fn from_stream_ticks_reduces_large_factor() {
    let tb = Fr::new(1000, 3000);
    let r = Rational::from_stream_ticks(9, tb).expect("ticks");
    assert_eq!(r.reduced(), Rational::new_raw(3, 1));
}

#[test]
fn from_stream_ticks_negative_pts() {
    let tb = Fr::new(1, 30_000);
    let r = Rational::from_stream_ticks(-60_000, tb).expect("ticks");
    assert_eq!(r.reduced(), Rational::new_raw(-2, 1));
}

#[test]
fn from_duration_ticks_positive_ok() {
    let tb = Fr::new(1, 1000);
    let d = Rational::from_duration_ticks(5000, tb).expect("duration");
    assert_eq!(d.reduced(), Rational::new_raw(5, 1));
}

#[test]
fn from_duration_ticks_small_positive() {
    let tb = Fr::new(1, 30_000);
    assert!(Rational::from_duration_ticks(1, tb).is_some());
}

#[test]
fn as_f64_zero_and_negative() {
    assert_eq!(Rational::new_raw(0, 17).as_f64(), 0.0);
    assert!((Rational::new_raw(-3, 8).as_f64() + 0.375).abs() < 1e-15);
}

#[test]
fn hash_consistent_with_eq() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    fn hx(r: Rational) -> u64 {
        let mut h = DefaultHasher::new();
        r.hash(&mut h);
        h.finish()
    }
    let a = Rational::new_raw(6, 9);
    let b = Rational::new_raw(6, 9);
    assert_eq!(a, b);
    assert_eq!(hx(a), hx(b));
}

#[test]
fn clone_roundtrip_eq() {
    let r = Rational::new_raw(-99, 7);
    assert_eq!(r, r.clone());
}

#[test]
fn copy_behaves_like_identity_for_ge() {
    let a = Rational::new_raw(5, 8);
    let b = Rational::new_raw(3, 8);
    let ac = a;
    assert!(ac.ge(b));
}

// --- Extra ordering cases (timeline-safe `ge` regression matrix) ---

ge_case!(ge_frac_1_5_vs_1_6, (1, 5), (1, 6), true);
ge_case!(ge_frac_1_6_vs_1_5, (1, 6), (1, 5), false);
ge_case!(ge_frac_1_7_vs_1_9, (1, 7), (1, 9), true);
ge_case!(ge_frac_2_9_vs_1_5, (2, 9), (1, 5), true);
ge_case!(ge_frac_1_5_vs_2_9, (1, 5), (2, 9), false);
ge_case!(ge_frac_22_7_vs_3, (22, 7), (3, 1), true);
ge_case!(ge_frac_3_vs_22_7, (3, 1), (22, 7), false);
ge_case!(ge_frac_neg_1_2_vs_neg_2_5, (-1, 2), (-2, 5), false);
ge_case!(ge_frac_neg_2_5_vs_neg_1_2, (-2, 5), (-1, 2), true);
ge_case!(ge_frac_10_1_vs_9_1, (10, 1), (9, 1), true);
ge_case!(ge_frac_100_13_vs_99_13, (100, 13), (99, 13), true);
ge_case!(ge_frac_same_value_diff_den, (6, 8), (9, 12), true);
ge_case!(ge_frac_same_value_diff_den_rev, (9, 12), (6, 8), true);
ge_case!(ge_mixed_7_4_vs_13_8, (7, 4), (13, 8), true);
ge_case!(ge_mixed_13_8_vs_7_4, (13, 8), (7, 4), false);
ge_case!(ge_small_den_high_precision, (1001, 1000), (1, 1), true);
ge_case!(ge_small_den_high_precision_rev, (1, 1), (1001, 1000), false);
ge_case!(ge_cross_large_denoms, (50_001, 99_999), (50_000, 99_999), true);
ge_case!(ge_whole_neg_vs_smaller_whole_neg, (-3, 1), (-10, 1), true);
ge_case!(ge_whole_neg_vs_larger_whole_neg, (-10, 1), (-3, 1), false);
ge_case!(ge_neg_frac_near_zero_vs_neg_one, (-1, 1_000_000), (-1, 1), true);
ge_case!(ge_one_vs_neg_frac_near_zero, (1, 1), (-1, 1_000_000), true);
ge_case!(ge_two_thirds_vs_one_half, (2, 3), (1, 2), true);
ge_case!(ge_one_half_vs_two_thirds, (1, 2), (2, 3), false);
ge_case!(ge_four_fifths_vs_three_quarters, (4, 5), (3, 4), true);
ge_case!(ge_three_quarters_vs_four_fifths, (3, 4), (4, 5), false);
ge_case!(ge_seven_eighths_vs_thirteen_sixteenths, (7, 8), (13, 16), true);
ge_case!(ge_thirteen_sixteenths_vs_seven_eighths, (13, 16), (7, 8), false);
ge_case!(ge_phi_approx_num, (13, 8), (8, 5), true);
ge_case!(ge_phi_approx_den, (8, 5), (13, 8), false);

#[test]
fn from_stream_ticks_zero_pts_is_zero_seconds() {
    let tb = Fr::new(1, 48_000);
    let r = Rational::from_stream_ticks(0, tb).expect("ticks");
    assert_eq!(r.reduced(), Rational::new_raw(0, 1));
}

#[test]
fn from_stream_ticks_large_tick_reduces() {
    let tb = Fr::new(1, 1000);
    let r = Rational::from_stream_ticks(1_000_000, tb).expect("ticks");
    assert_eq!(r.reduced(), Rational::new_raw(1000, 1));
}

#[test]
fn reduced_collapses_zero_numerator() {
    assert_eq!(
        Rational::new_raw(0, 81).reduced(),
        Rational::new_raw(0, 1)
    );
}

#[test]
fn reduced_even_large_even_components() {
    assert_eq!(
        Rational::new_raw(1_002, 2_004).reduced(),
        Rational::new_raw(1, 2)
    );
}

#[test]
fn reduced_coprime_large_primes_style() {
    let r = Rational::new_raw(1_009, 1_013).reduced();
    assert_eq!(r.num, 1009);
    assert_eq!(r.den, 1013);
}

#[test]
fn from_duration_ticks_rejects_negative_ticks_even_if_tb_ok() {
    let tb = Fr::new(1, 1000);
    assert!(Rational::from_duration_ticks(-5, tb).is_none());
}

#[test]
fn as_f64_numerator_almost_denominator() {
    let r = Rational::new_raw(1_000_000_000, 1_000_000_001);
    assert!(r.as_f64() > 0.999 && r.as_f64() < 1.0);
}

#[test]
fn debug_rational_is_readable() {
    let s = format!("{:?}", Rational::new_raw(-5, 12));
    assert!(s.contains("-5") && s.contains("12"));
}

#[test]
fn from_stream_ticks_huge_product_returns_none_when_seconds_out_of_range() {
    let tb = Fr::new(i32::MAX, 1);
    assert!(
        Rational::from_stream_ticks(i64::MAX, tb).is_none(),
        "normalized seconds must fit i64"
    );
}
