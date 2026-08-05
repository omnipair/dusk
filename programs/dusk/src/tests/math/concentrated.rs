use super::*;
use proptest::prelude::*;

#[allow(clippy::assign_op_pattern, clippy::manual_div_ceil)]
mod reference_wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U256(4);
    }
}

use reference_wide::U256;

const PEAK_DEPTH_200: u128 = 200 * NAD as u128;
const FADE_TENTH: u128 = NAD as u128 / 10;

fn analytical_inner_residual(x: f64, y: f64, d: f64, peak_depth: f64, fade: f64) -> f64 {
    let q = 4.0 * x * y / (d * d);
    let delta = 1.0 - q;
    let weight = (fade / (fade + delta)).powi(2);
    2.0 * peak_depth * q * weight * ((x + y - d) / d) + q - 1.0
}

fn reserves_at_c1_coordinate(d: u128, q_q48: u128, v_q48: u128) -> (u128, u128) {
    let sqrt_q = isqrt(q_q48.checked_mul(Q48_ONE).unwrap());
    let cosh = isqrt(Q48_ONE * Q48_ONE + v_q48 * v_q48);
    let low_factor = mul_q48(sqrt_q, cosh - v_q48).unwrap();
    let high_factor = mul_q48(sqrt_q, cosh + v_q48).unwrap();
    let denominator = Q48_ONE * 2;
    (
        mul_div_floor(d, low_factor, denominator).unwrap().max(1),
        mul_div_floor(d, high_factor, denominator).unwrap().max(1),
    )
}

fn inner_shape_residual_q80(q_q80: u128, v_q80: u128, peak_depth_nad: u128, fade_scale_nad: u128) -> bool {
    let peak_q80 = mul_div_floor(peak_depth_nad, Q80_ONE, NAD as u128).unwrap();
    let scale_q80 = mul_div_floor(fade_scale_nad, Q80_ONE, NAD as u128).unwrap();
    let sqrt_q = sqrt_q80(q_q80).unwrap();
    let cosh = sqrt_q80(Q80_ONE + mul_q80(v_q80, v_q80).unwrap()).unwrap();
    let h = mul_q80(sqrt_q, cosh).unwrap().saturating_sub(Q80_ONE);
    let delta = Q80_ONE - q_q80;
    let weight_base = div_q80(scale_q80, scale_q80 + delta).unwrap();
    let coefficient = mul_q80(2 * peak_q80, mul_q80(q_q80, mul_q80(weight_base, weight_base).unwrap()).unwrap())
        .unwrap();
    mul_q80(coefficient, h).unwrap() + q_q80 >= Q80_ONE
}

fn solve_inner_q_at_v_q80(v_q80: u128, q_floor: u128, peak_depth_nad: u128, fade_scale_nad: u128) -> u128 {
    let mut low = q_floor;
    let mut high = Q80_ONE;
    assert!(!inner_shape_residual_q80(
        low,
        v_q80,
        peak_depth_nad,
        fade_scale_nad
    ));
    assert!(inner_shape_residual_q80(
        high,
        v_q80,
        peak_depth_nad,
        fade_scale_nad
    ));
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        if inner_shape_residual_q80(midpoint, v_q80, peak_depth_nad, fade_scale_nad) {
            high = midpoint;
        } else {
            low = midpoint;
        }
    }
    high
}

fn outward_branch_stage(branch: ConcentratedHybridBranch, direction: ConcentratedSwapDirection) -> u8 {
    match (direction, branch) {
        (_, ConcentratedHybridBranch::Inner) => 0,
        (ConcentratedSwapDirection::BaseToQuote, ConcentratedHybridBranch::QuoteScarceTransition)
        | (ConcentratedSwapDirection::QuoteToBase, ConcentratedHybridBranch::BaseScarceTransition) => 1,
        (ConcentratedSwapDirection::BaseToQuote, ConcentratedHybridBranch::QuoteScarceTail)
        | (ConcentratedSwapDirection::QuoteToBase, ConcentratedHybridBranch::BaseScarceTail) => 2,
        _ => panic!("outward quote entered the opposite scarcity branch: {branch:?}"),
    }
}

fn raw_quote_endpoint(
    prepared: ConcentratedPreparedCurve,
    amount_in: u128,
    direction: ConcentratedSwapDirection,
) -> (u128, u128, u128, ConcentratedHybridBranch) {
    let output = prepared.quote_exact_in(amount_in, direction).unwrap();
    let (base_after, quote_after) = match direction {
        ConcentratedSwapDirection::BaseToQuote => (
            prepared.base_reserve_nad() + amount_in,
            prepared.quote_reserve_nad() - output,
        ),
        ConcentratedSwapDirection::QuoteToBase => (
            prepared.base_reserve_nad() - output,
            prepared.quote_reserve_nad() + amount_in,
        ),
    };
    let branch = prepared
        .hybrid_branch_at_raw_reserves(base_after, quote_after)
        .unwrap();
    (output, base_after, quote_after, branch)
}

fn first_raw_input_at_outward_stage(
    prepared: ConcentratedPreparedCurve,
    direction: ConcentratedSwapDirection,
    target_stage: u8,
) -> u128 {
    let mut low = 0_u128;
    let mut high = 1_u128;
    while outward_branch_stage(raw_quote_endpoint(prepared, high, direction).3, direction) < target_stage {
        low = high;
        high = high.checked_mul(2).expect("raw join crossing bound");
    }
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        if outward_branch_stage(raw_quote_endpoint(prepared, midpoint, direction).3, direction) >= target_stage {
            high = midpoint;
        } else {
            low = midpoint;
        }
    }
    high
}

fn assert_canonical_invariant(prepared: ConcentratedPreparedCurve) {
    let high = prepared.invariant_d();
    if prepared.peak_depth_nad() == 0 {
        return;
    }
    let (canonical_sign, canonical_magnitude) = hybrid_residual(
        prepared.base_common,
        prepared.quote_common,
        high,
        prepared.geometry,
    )
    .unwrap();
    if canonical_sign {
        assert_eq!(canonical_magnitude, 0);
    } else {
        let adjacent = high.checked_sub(1).unwrap();
        assert!(
            hybrid_residual(
                prepared.base_common,
                prepared.quote_common,
                adjacent,
                prepared.geometry,
            )
            .unwrap()
            .0
        );
    }
}

#[test]
fn cpmm_mode_is_bit_exact_for_quotes() {
    let x = 9_000_000_000_000_u128;
    let y = 13_000_000_000_000_u128;
    let dx = 71_000_000_000_u128;
    let dy = 43_000_000_000_u128;

    assert_eq!(
        concentrated_quote_exact_in(x, y, dx, ConcentratedSwapDirection::BaseToQuote, NAD as u128, 0, 0,).unwrap(),
        calculate_normalized_amount_out(x, y, dx).unwrap()
    );
    assert_eq!(
        concentrated_quote_exact_out(x, y, dy, ConcentratedSwapDirection::BaseToQuote, NAD as u128, 0, 0,).unwrap(),
        calculate_normalized_amount_in(x, y, dy).unwrap()
    );
}

#[test]
fn cpmm_balanced_equivalent_q_is_center_independent() {
    let base = 1_000_u128 * NAD as u128;
    let quote = 4_000_u128 * NAD as u128;
    let centered = concentrated_prepare_curve(base, quote, 4_u128 * NAD as u128, 0, 0).unwrap();
    let stale_center = concentrated_prepare_curve(base, quote, 2_u128 * NAD as u128, 0, 0).unwrap();

    assert_eq!(centered.invariant_d(), 8_000_u128 * NAD as u128);
    assert_ne!(centered.invariant_d(), stale_center.invariant_d());
    assert_eq!(centered.balanced_equivalent_q().unwrap(), 2_000_u128 * NAD as u128);
    assert_eq!(stale_center.balanced_equivalent_q().unwrap(), 2_000_u128 * NAD as u128);
}

#[test]
fn concentrated_balanced_equivalent_q_is_exact_across_wide_radicands() {
    for (invariant_d, center_price_nad) in [
        (2_u128 * NAD as u128, 1_u128),
        (2_u128 * u64::MAX as u128, 1_u128),
        (2_u128 * u64::MAX as u128 - 1, NAD as u128),
        (2_u128 * u64::MAX as u128 - 1, u64::MAX as u128),
    ] {
        let prepared = ConcentratedPreparedCurve {
            base_reserve_nad: NAD as u128,
            quote_reserve_nad: NAD as u128,
            base_common: NAD as u128,
            quote_common: NAD as u128,
            center_price_nad,
            peak_depth_nad: NAD as u128,
            fade_scale_nad: NAD as u128 / 10,
            invariant_d,
            common_numeraire: ConcentratedCommonNumeraire::for_center(center_price_nad).unwrap(),
            geometry: Some(ConcentratedC1Geometry::derive(NAD as u128, NAD as u128 / 10).unwrap()),
        };
        let q = prepared.balanced_equivalent_q().unwrap();
        let (ratio_numerator, ratio_denominator) = if center_price_nad >= NAD as u128 {
            (NAD as u128, center_price_nad * 4)
        } else {
            (center_price_nad, 4 * NAD as u128)
        };
        let denominator = U256::from(ratio_denominator);
        let radicand_numerator = U256::from(invariant_d)
            * U256::from(invariant_d)
            * U256::from(ratio_numerator);
        let square = U256::from(q) * U256::from(q);
        let successor = U256::from(q + 1) * U256::from(q + 1);

        assert!(square * denominator <= radicand_numerator);
        assert!(successor * denominator > radicand_numerator);
    }
}

#[test]
fn balanced_concentrated_state_has_exact_center_mark() {
    let reserve = 4_000_000_000_000_u128;
    let prepared = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    assert_eq!(prepared.invariant_d(), 2 * reserve);
    assert_eq!(prepared.marginal_price_nad().unwrap(), NAD as u128);
}

#[test]
fn q64_inner_residual_has_the_analytical_sign() {
    for (x, y) in [
        (900_000_000_000_u128, 1_100_000_000_000_u128),
        (1_000_000_000_000, 1_150_000_000_000),
        (1_150_000_000_000, 1_000_000_000_000),
    ] {
        assert_eq!(
            concentrated_hybrid_branch_from_common(x, y, PEAK_DEPTH_200, FADE_TENTH).unwrap(),
            ConcentratedHybridBranch::Inner
        );
        let geometric = geometric_lower_d(x, y).unwrap();
        let sum = x + y;
        for fraction in [0.15_f64, 0.35, 0.6, 0.85] {
            let d = geometric + ((sum - geometric) as f64 * fraction) as u128;
            let analytical = analytical_inner_residual(x as f64, y as f64, d as f64, 200.0, 0.1);
            let integer_positive = hybrid_residual(
                x,
                y,
                d,
                Some(ConcentratedC1Geometry::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap()),
            )
            .unwrap()
            .0;
            assert_eq!(integer_positive, analytical >= 0.0, "x={x} y={y} d={d}");
        }
    }
}

#[test]
fn q64_inner_quotes_remove_the_full_scale_q48_staircase() {
    let peak = 2 * NAD as u128;
    let fade = NAD as u128 / 10;
    for (reserve, first_input) in [
        (1_000_000_000_000_000_u128, 10_u128),
        (1_000_000_000_000_000_000, 10_658),
        (18_446_744_073_709_051_615, 131_071),
        (18_446_744_073_709_051_615, 196_607),
    ] {
        let prepared = concentrated_prepare_curve(reserve, reserve, NAD as u128, peak, fade).unwrap();
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            let first = prepared.quote_exact_in(first_input, direction).unwrap();
            let adjacent = prepared.quote_exact_in(first_input + 1, direction).unwrap();
            assert!(
                adjacent >= first && adjacent - first <= 1,
                "reserve={reserve} direction={direction:?} input={first_input} first={first} adjacent={adjacent}"
            );
        }
    }
}

#[test]
fn full_precision_inner_to_transition_join_is_monotone_and_split_resistant_at_max_scale() {
    for (peak, fade, base, quote, input_before) in [
        (
            2 * NAD as u128,
            100_u128,
            18_446_744_073_708_951_614_u128,
            18_438_960_294_243_435_255_u128,
            49_995_u128,
        ),
        (
            2_000 * NAD as u128,
            199_000_000_u128,
            18_446_744_073_708_951_614_u128,
            11_716_330_836_516_057_264_u128,
            38_857_u128,
        ),
    ] {
        let prepared = concentrated_prepare_curve(base, quote, NAD as u128, peak, fade).unwrap();
        let quote_at = |input| {
            let output = prepared
                .quote_exact_in(input, ConcentratedSwapDirection::BaseToQuote)
                .unwrap();
            let branch = concentrated_hybrid_branch_from_common(base + input, quote - output, peak, fade).unwrap();
            (output, branch)
        };
        let mut low = 1_u128;
        let mut high = input_before.max(2);
        while quote_at(high).1 == ConcentratedHybridBranch::Inner {
            low = high;
            high = high.checked_mul(2).expect("transition crossing bound");
        }
        while high - low > 1 {
            let midpoint = low + (high - low) / 2;
            if quote_at(midpoint).1 == ConcentratedHybridBranch::Inner {
                low = midpoint;
            } else {
                high = midpoint;
            }
        }
        let crossing_input = high;
        let (after, after_branch) = quote_at(crossing_input);
        assert_eq!(after_branch, ConcentratedHybridBranch::QuoteScarceTransition);
        let before = prepared
            .quote_exact_in(crossing_input - 1, ConcentratedSwapDirection::BaseToQuote)
            .unwrap();
        let base_before = base + crossing_input - 1;
        let base_after = base_before + 1;
        assert_eq!(
            concentrated_hybrid_branch_from_common(base_before, quote - before, peak, fade).unwrap(),
            ConcentratedHybridBranch::Inner
        );
        assert_eq!(
            concentrated_hybrid_branch_from_common(base_after, quote - after, peak, fade).unwrap(),
            ConcentratedHybridBranch::QuoteScarceTransition
        );
        assert!(after >= before);
        let split_state = concentrated_prepare_curve(
            base_before,
            quote - before,
            NAD as u128,
            peak,
            fade,
        )
        .unwrap();
        let split_tail = split_state
            .quote_exact_in(1, ConcentratedSwapDirection::BaseToQuote)
            .unwrap_or(0);
        assert!(
            before + split_tail <= after,
            "peak={peak} fade={fade} crossing_input={crossing_input} one_shot={after} split={}+{split_tail}",
            before
        );
    }
}

#[test]
fn full_precision_join_matches_aggressive_discrete_reference_and_blocks_split_gain() {
    let peak = 2_000 * NAD as u128;
    let fade = 1_000_000_u128;
    let base = 18_446_744_073_708_951_614_u128;
    let quote = 17_872_265_831_270_771_237_u128;
    let input = 49_214_u128;
    let prepared = concentrated_prepare_curve(base, quote, NAD as u128, peak, fade).unwrap();
    let one_shot = prepared
        .quote_exact_in(input, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    assert_eq!(prepared.invariant_d(), 36_319_006_357_314_842_051);
    assert_eq!(one_shot, 49_212, "independent continuous floor");

    let first_input = 49_212_u128;
    let first = prepared
        .quote_exact_in(first_input, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    let second_curve = concentrated_prepare_curve(
        base + first_input,
        quote - first,
        NAD as u128,
        peak,
        fade,
    )
    .unwrap();
    let second = second_curve
        .quote_exact_in(input - first_input, ConcentratedSwapDirection::BaseToQuote)
        .unwrap_or(0);
    assert_eq!((first, second), (49_210, 1));
    assert!(first + second <= one_shot, "one_shot={one_shot} split={first}+{second}");

    let reverse_curve = concentrated_prepare_curve(
        base + input,
        quote - one_shot,
        NAD as u128,
        peak,
        fade,
    )
    .unwrap();
    let roundtrip = reverse_curve
        .quote_exact_in(one_shot, ConcentratedSwapDirection::QuoteToBase)
        .unwrap();
    assert_eq!(roundtrip, 49_212);
    assert!(roundtrip <= input, "input={input} roundtrip={roundtrip}");
}

#[test]
fn full_precision_transition_closes_previous_small_trade_split_regression() {
    let peak = 512_769_412_294_u128;
    let fade = 25_212_680_u128;
    // Leave headroom for the exact-input addition while preserving the
    // full-scale reserve ratio that previously exposed split advantage.
    let base = u64::MAX as u128 - 1_000_000;
    let quote = 15_729_292_133_186_232_478_u128;
    let input = 257_u128;
    let prepared = concentrated_prepare_curve(base, quote, NAD as u128, peak, fade).unwrap();
    let one_shot = prepared
        .quote_exact_in(input, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    let first = prepared
        .quote_exact_in(1, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    let second = concentrated_prepare_curve(base + 1, quote - first, NAD as u128, peak, fade)
        .unwrap()
        .quote_exact_in(input - 1, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    assert!(first + second <= one_shot, "one_shot={one_shot} split={first}+{second}");

    let returned = concentrated_prepare_curve(base + input, quote - one_shot, NAD as u128, peak, fade)
        .unwrap()
        .quote_exact_in(one_shot, ConcentratedSwapDirection::QuoteToBase)
        .unwrap();
    assert!(returned <= input);
}

#[test]
fn restoring_trade_in_exact_tail_is_bit_identical_to_cpmm() {
    let peak = 639_301_941_594_u128;
    let fade = 150_212_273_u128;
    let base = 77_854_937_841_089_103_u128;
    let quote = 1_152_921_504_606_846_975_u128;
    let input = 77_854_937_841_u128;
    let prepared = concentrated_prepare_curve(base, quote, NAD as u128, peak, fade).unwrap();
    assert_eq!(
        prepared
            .hybrid_branch_at_raw_reserves(base, quote)
            .unwrap(),
        ConcentratedHybridBranch::BaseScarceTail
    );
    assert_eq!(
        prepared
            .quote_exact_in(input, ConcentratedSwapDirection::BaseToQuote)
            .unwrap(),
        calculate_normalized_amount_out(base, quote, input).unwrap()
    );
}

#[test]
fn full_precision_transition_conditioning_stays_below_unit_radial_slope() {
    let parameter_grid = [
        (1_u128, 100_u128),
        (2 * NAD as u128 / 9_000, 100_u128),
        (NAD as u128, 50_u128),
        (3 * NAD as u128 / 2, 75),
        (2 * NAD as u128, 100),
        (2 * NAD as u128, NAD as u128 / 10),
        (10 * NAD as u128, NAD as u128 / 1_000),
        (200 * NAD as u128, NAD as u128 / 10),
        (CONCENTRATED_MAX_PEAK_DEPTH_NAD, CONCENTRATED_MAX_FADE_SCALE_NAD),
    ];
    for (peak, fade) in parameter_grid {
        let geometry = ConcentratedC1Geometry::derive(peak, fade).unwrap();
        let transition_width = geometry.v_tail_q80 - geometry.v_start_q80;
        for step in 0..=256_u128 {
            let v = geometry.v_start_q80 + mul_div_floor(transition_width, step, 256).unwrap();
            let (q, negative_slope) = geometry.transition_q_and_slope_at_v_q80(v).unwrap();
            let cosh = sqrt_q80(Q80_ONE + mul_q80(v, v).unwrap()).unwrap();

            // r=(-q'(v)*cosh(v))/(2q).  The U256 comparison keeps this
            // conditioning gate independent of fixed-point multiplication
            // rounding.  r<1 makes the continuous reserve solve one-to-one.
            let numerator = U256::from(negative_slope) * U256::from(cosh);
            let denominator = U256::from(q) * U256::from(Q80_ONE) * U256::from(2_u8);
            assert!(
                numerator < denominator,
                "peak={peak} fade={fade} step={step} q={q} slope={negative_slope} cosh={cosh}"
            );
        }
    }
}

#[test]
fn q64_inner_is_monotone_at_max_scale_and_ramp_intermediates() {
    let reserve = 18_446_744_073_709_051_615_u128;
    let inputs = [
        1_u128, 2, 3, 15, 255, 4_095, 65_535, 131_071, 196_607, 262_143,
    ];
    for (peak, fade) in [
        (1_u128, 100_u128),
        (2 * NAD as u128 / 9_000, 100_u128),
        (NAD as u128, 50_u128),
        (3 * NAD as u128 / 2, 75),
        (2 * NAD as u128 - 1, 99),
        (2 * NAD as u128, 100),
        (2 * NAD as u128, NAD as u128 / 10),
        (CONCENTRATED_MAX_PEAK_DEPTH_NAD, CONCENTRATED_MAX_FADE_SCALE_NAD),
    ] {
        let prepared = concentrated_prepare_curve(reserve, reserve, NAD as u128, peak, fade).unwrap();
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            for input in inputs {
                let output = prepared.quote_exact_in(input, direction).unwrap();
                let adjacent = prepared.quote_exact_in(input + 1, direction).unwrap();
                assert!(
                    adjacent >= output && adjacent - output <= 1,
                    "peak={peak} fade={fade} direction={direction:?} input={input} output={output} adjacent={adjacent}"
                );
            }
        }
    }
}

#[test]
fn raw_center_normalization_certifies_actual_endpoints_in_both_directions() {
    let target_common = 1_000_000_000_000_000_u128;
    for center in [
        1_u128,
        123_456_789,
        NAD as u128 / 3,
        3 * NAD as u128 / 2,
        u64::MAX as u128,
    ] {
        let (base, quote) = if center >= NAD as u128 {
            (
                mul_div_ceil_u128(target_common, NAD as u128, center).unwrap(),
                target_common,
            )
        } else {
            (
                target_common,
                mul_div_ceil_u128(target_common, center, NAD as u128).unwrap(),
            )
        };
        let prepared = concentrated_prepare_curve(base, quote, center, PEAK_DEPTH_200, FADE_TENTH).unwrap();
        for (direction, input) in [
            (ConcentratedSwapDirection::BaseToQuote, (base / 100_000).max(1)),
            (ConcentratedSwapDirection::QuoteToBase, quote / 100),
        ] {
            let output = prepared.quote_exact_in(input, direction).unwrap();
            assert!(output > 0, "center={center} direction={direction:?}");
        }

        for (direction, requested) in [
            (ConcentratedSwapDirection::BaseToQuote, quote / 1_000_000),
            (ConcentratedSwapDirection::QuoteToBase, (base / 1_000_000).max(1)),
        ] {
            let input = concentrated_quote_exact_out(
                base,
                quote,
                requested,
                direction,
                center,
                PEAK_DEPTH_200,
                FADE_TENTH,
            )
            .unwrap();
            let replay = prepared.quote_exact_in(input, direction).unwrap();
            assert!(
                replay >= requested,
                "center={center} direction={direction:?} input={input} replay={replay} requested={requested}"
            );
            if input > 1 {
                let predecessor = prepared.quote_exact_in(input - 1, direction).unwrap();
                assert!(
                    predecessor < requested,
                    "center={center} direction={direction:?} predecessor={} replay={predecessor} requested={requested}",
                    input - 1
                );
            }
        }
    }
}

#[test]
fn adaptive_common_scale_inverse_floor_is_exact_at_center_boundaries() {
    for center in [
        1_u128,
        NAD as u128 - 1,
        NAD as u128,
        NAD as u128 + 1,
        u64::MAX as u128,
    ] {
        let numeraire = ConcentratedCommonNumeraire::for_center(center).unwrap();
        for scale in [numeraire.base_scale(center).unwrap(), numeraire.quote_scale(center).unwrap()] {
            assert!(scale.numerator() >= scale.denominator());
            for target_common in [
                0_u128,
                1,
                2,
                NAD as u128 - 1,
                NAD as u128,
                1_000_000_000_000_000,
                MAX_COMMON_RESERVE,
            ] {
                let raw = scale.common_to_raw_ceil(target_common).unwrap();
                assert!(
                    scale.to_common_floor(raw).unwrap() >= target_common,
                    "center={center} scale={scale:?} target={target_common} raw={raw}"
                );
                if raw > 0 {
                    assert!(
                        scale.to_common_floor(raw - 1).unwrap() < target_common,
                        "center={center} scale={scale:?} target={target_common} raw={raw}"
                    );
                }
            }
        }
    }
}

#[test]
fn low_center_quotes_keep_single_raw_atoms_economically_live() {
    let center = 1_u128;
    let base = 1_000_000_000_000_000_000_u128;
    let quote = NAD as u128;
    let prepared = concentrated_prepare_curve(base, quote, center, 2 * NAD as u128, FADE_TENTH).unwrap();

    assert_eq!(prepared.common_numeraire(), ConcentratedCommonNumeraire::Base);
    assert_eq!((prepared.base_common, prepared.quote_common), (base, base));

    let output = prepared
        .quote_exact_in(1, ConcentratedSwapDirection::QuoteToBase)
        .unwrap();
    assert_eq!(output, 999_999_999);
    assert_eq!(
        prepared
            .quote_exact_out_input_bracket(output, ConcentratedSwapDirection::QuoteToBase)
            .unwrap(),
        (0, 1)
    );

    let reverse = concentrated_prepare_curve(base - output, quote + 1, center, 2 * NAD as u128, FADE_TENTH)
        .unwrap()
        .quote_exact_in(output, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    assert!(reverse <= 1);
}

#[test]
fn adaptive_numeraire_crossing_is_quote_continuous_and_exact_out_replays() {
    let reserve = 1_000_000_000_000_000_u128;
    let input = 1_000_000_000_u128;
    let mut outputs = [[0_u128; 2]; 3];
    for (center_index, center) in [NAD as u128 - 1, NAD as u128, NAD as u128 + 1]
        .into_iter()
        .enumerate()
    {
        let prepared = concentrated_prepare_curve(reserve, reserve, center, PEAK_DEPTH_200, FADE_TENTH).unwrap();
        for (direction_index, direction) in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ]
        .into_iter()
        .enumerate()
        {
            let output = prepared.quote_exact_in(input, direction).unwrap();
            assert!(output > 0);
            outputs[center_index][direction_index] = output;

            let exact_out = prepared.quote_exact_out_input_bracket(output, direction).unwrap();
            assert!(prepared.quote_exact_in(exact_out.1, direction).unwrap() >= output);
            if exact_out.0 > 0 {
                assert!(prepared.quote_exact_in(exact_out.0, direction).unwrap() < output);
            }
        }
    }
    for direction_index in 0..2 {
        assert!(outputs[0][direction_index].abs_diff(outputs[1][direction_index]) <= 3);
        assert!(outputs[1][direction_index].abs_diff(outputs[2][direction_index]) <= 3);
    }
}

#[test]
fn invariant_roots_are_canonical_adjacent_atoms() {
    for (x, y) in [
        (1_000_000_000_000_u128, 1_010_000_000_000_u128),
        (1_000_000_000_000, 1_500_000_000_000),
        (8_000_000_000_000, 1_000_000_000),
        (1_000_000_000, 8_000_000_000_000),
    ] {
        assert_canonical_invariant(concentrated_prepare_curve(x, y, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap());
    }
}

#[test]
fn invariant_solver_stays_inside_the_fixed_budget() {
    reset_residual_evaluations();
    reset_sqrt_q80_evaluations();
    let prepared = concentrated_prepare_curve(
        1_000_000_000_000,
        1_350_000_000_000,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    // Two roots define the protocol geometry and one root derives this
    // reserve state's shape. The invariant bracket reuses that shape through
    // every D probe instead of recomputing a square root per iteration.
    assert_eq!(sqrt_q80_evaluations(), 3);
    assert_canonical_invariant(prepared);
    assert!(residual_evaluations() <= CONCENTRATED_INVARIANT_MAX_ITERS + 4);
}

#[test]
fn ordinary_inner_quote_uses_secant_bracketing_without_q80_fallback() {
    let reserve = 1_000_000_000_000_000_u128;
    let prepared = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    reset_residual_evaluations();
    reset_sqrt_q80_evaluations();
    reset_q80_fallback_evaluations();

    let output = prepared
        .quote_exact_in(reserve / 1_000, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    assert!(output > 0);
    assert!(
        residual_evaluations() <= 32,
        "ordinary inner quote used {} residual probes",
        residual_evaluations()
    );
    assert_eq!(sqrt_q80_evaluations(), 0);
    assert_eq!(q80_fallback_evaluations(), 0);
}

#[test]
fn transition_quote_uses_bounded_newton_work_without_q80_fallback() {
    let prepared = concentrated_prepare_curve(
        100_000_000_000,
        200_000_000_000,
        2 * NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    reset_residual_evaluations();
    reset_sqrt_q64_evaluations();
    reset_sqrt_q80_evaluations();
    reset_q80_fallback_evaluations();

    let output = prepared
        .quote_exact_in(30_000_000_000, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    let branch = prepared
        .hybrid_branch_at_raw_reserves(130_000_000_000, 200_000_000_000 - output)
        .unwrap();
    assert!(output > 0);
    assert_eq!(branch, ConcentratedHybridBranch::QuoteScarceTransition);
    assert!(residual_evaluations() <= 8);
    assert!(sqrt_q64_evaluations() <= 5);
    assert_eq!(sqrt_q80_evaluations(), 0);
    assert_eq!(q80_fallback_evaluations(), 0);
}

#[test]
fn both_exact_cpmm_tails_are_reachable() {
    assert_eq!(
        concentrated_hybrid_branch_from_common(8_000_000_000_000, 1_000_000_000, PEAK_DEPTH_200, FADE_TENTH,).unwrap(),
        ConcentratedHybridBranch::QuoteScarceTail
    );
    assert_eq!(
        concentrated_hybrid_branch_from_common(1_000_000_000, 8_000_000_000_000, PEAK_DEPTH_200, FADE_TENTH,).unwrap(),
        ConcentratedHybridBranch::BaseScarceTail
    );
}

#[test]
fn same_tail_swap_is_exact_raw_cpmm() {
    let x = 8_000_000_000_000_u128;
    let y = 1_000_000_000_u128;
    let dx = 100_000_000_u128;
    let expected = calculate_normalized_amount_out(x, y, dx).unwrap();
    let actual = concentrated_quote_exact_in(
        x,
        y,
        dx,
        ConcentratedSwapDirection::BaseToQuote,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn exact_in_is_monotone_at_the_tail_to_transition_boundary() {
    let base = 8_000_000_000_000_u128;
    let quote = 1_000_000_000_u128;
    let direction = ConcentratedSwapDirection::QuoteToBase;
    let mut low = 0_u128;
    let mut high = quote;
    while exact_cpmm_tail_in_raw(
        base,
        quote,
        high,
        direction,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap()
    .is_some()
    {
        low = high;
        high = high.checked_mul(2).unwrap();
    }
    while high - low > 1 {
        let probe = low + (high - low) / 2;
        if exact_cpmm_tail_in_raw(
            base,
            quote,
            probe,
            direction,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        )
        .unwrap()
        .is_some()
        {
            low = probe;
        } else {
            high = probe;
        }
    }

    let last_tail_output = concentrated_quote_exact_in(
        base,
        quote,
        low,
        direction,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    let first_crossing_output = concentrated_quote_exact_in(
        base,
        quote,
        high,
        direction,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();

    assert_eq!(high, low + 1);
    assert!(
        first_crossing_output >= last_tail_output,
        "adding one input atom at the branch crossing reduced output: input {low}->{high}, output {last_tail_output}->{first_crossing_output}"
    );
    let former_whole_trade_haircut = (first_crossing_output * 10).div_ceil(1_000_000);
    assert!(
        first_crossing_output - former_whole_trade_haircut < last_tail_output,
        "fixture must reproduce the removed 10 ppm branch-crossing discontinuity"
    );

    let base_after_first = base.checked_sub(last_tail_output).unwrap();
    let quote_after_first = quote.checked_add(low).unwrap();
    let split_output = last_tail_output
        .checked_add(
            concentrated_quote_exact_in(
                base_after_first,
                quote_after_first,
                1,
                direction,
                NAD as u128,
                PEAK_DEPTH_200,
                FADE_TENTH,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        first_crossing_output >= split_output,
        "splitting exactly at the branch crossing improved output: one-shot={first_crossing_output} split={split_output}"
    );
}

#[test]
fn concentrated_exact_in_selects_the_maximal_safe_output_atom() {
    let base = 1_000_000_000_000_u128;
    let quote = 1_100_000_000_000_u128;
    let input = 50_000_000_000_u128;
    let prepared = concentrated_prepare_curve(
        base,
        quote,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    let output = prepared
        .quote_exact_in(input, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    let x_after = prepared.base_common.checked_add(input).unwrap();
    let y_after = prepared.quote_common.checked_sub(output).unwrap();

    assert!(
        hybrid_residual(
            x_after,
            y_after,
            prepared.invariant_d(),
            prepared.geometry,
        )
        .unwrap()
        .0
    );
    assert!(
        !hybrid_residual(
            x_after,
            y_after - 1,
            prepared.invariant_d(),
            prepared.geometry,
        )
        .unwrap()
        .0,
        "one additional output atom must be on the invalid side"
    );
}

#[test]
fn convergence_transition_matches_value_and_slope_at_both_joins() {
    let geometry = ConcentratedC1Geometry::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let (start_q, start_slope) = geometry
        .transition_q_and_slope_at_v(geometry.v_start_q48)
        .unwrap();
    let (tail_q, tail_slope) = geometry
        .transition_q_and_slope_at_v(geometry.v_tail_q48)
        .unwrap();
    assert_eq!(start_q, geometry.q_start_q48);
    assert_eq!(start_slope, geometry.negative_q_prime_start_q48);
    assert_eq!(tail_q, geometry.q_tail_q48);
    assert_eq!(tail_slope, 0);

    let d = 2_000_000_000_000_u128;
    let (tail_low, tail_high) = reserves_at_c1_coordinate(d, tail_q, geometry.v_tail_q48);
    let reconstructed =
        concentrated_prepare_curve(tail_high, tail_low, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    assert!(reconstructed.invariant_d().abs_diff(d) <= 100_000);
    let one_step_out = reserves_at_c1_coordinate(d, tail_q, geometry.v_tail_q48 + 1);
    assert!(concentrated_hybrid_branch_from_common(
        one_step_out.1,
        one_step_out.0,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap()
    .is_exact_tail());
}

#[test]
fn q80_inner_transition_and_tail_have_matching_one_sided_derivatives() {
    for (peak, fade) in [
        (2 * NAD as u128, NAD as u128 / 10),
        (PEAK_DEPTH_200, FADE_TENTH),
        (CONCENTRATED_MAX_PEAK_DEPTH_NAD, CONCENTRATED_MAX_FADE_SCALE_NAD),
    ] {
        let geometry = ConcentratedC1Geometry::derive(peak, fade).unwrap();
        let transition_width = geometry.v_tail_q80 - geometry.v_start_q80;
        let epsilon = (transition_width / 1_000_000).max(1);
        let inner_v = geometry.v_start_q80 - epsilon;
        let inner_q = solve_inner_q_at_v_q80(inner_v, geometry.q_start_q80, peak, fade);
        let transition_q = geometry
            .transition_q_and_slope_at_v_q80(geometry.v_start_q80 + epsilon)
            .unwrap()
            .0;
        let inner_slope = mul_div_floor(inner_q - geometry.q_start_q80, Q80_ONE, epsilon).unwrap();
        let transition_slope = mul_div_floor(geometry.q_start_q80 - transition_q, Q80_ONE, epsilon).unwrap();
        let slope_error = inner_slope.abs_diff(transition_slope);
        assert!(
            slope_error.saturating_mul(1_000) <= inner_slope.max(transition_slope),
            "peak={peak} fade={fade} inner={inner_slope} transition={transition_slope}"
        );

        let tail_q = geometry
            .transition_q_and_slope_at_v_q80(geometry.v_tail_q80 - epsilon)
            .unwrap()
            .0;
        let tail_slope = mul_div_floor(tail_q - geometry.q_tail_q80, Q80_ONE, epsilon).unwrap();
        assert!(
            tail_slope.saturating_mul(1_000) <= geometry.negative_q_prime_start_q80,
            "peak={peak} fade={fade} tail={tail_slope} start={} ",
            geometry.negative_q_prime_start_q80
        );
    }
}

#[test]
fn raw_quotes_are_c1_continuous_at_both_joins_across_center_orientations() {
    let target_common = 1_000_000_000_000_000_u128;
    for center in [1_u128, NAD as u128, u64::MAX as u128] {
        let numeraire = ConcentratedCommonNumeraire::for_center(center).unwrap();
        let base = numeraire
            .base_scale(center)
            .unwrap()
            .common_to_raw_ceil(target_common)
            .unwrap();
        let quote = numeraire
            .quote_scale(center)
            .unwrap()
            .common_to_raw_ceil(target_common)
            .unwrap();
        let prepared = concentrated_prepare_curve(base, quote, center, PEAK_DEPTH_200, FADE_TENTH).unwrap();
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            for target_stage in [1_u8, 2_u8] {
                let crossing = first_raw_input_at_outward_stage(prepared, direction, target_stage);
                let before = prepared.quote_exact_in(crossing - 1, direction).unwrap();
                let at = prepared.quote_exact_in(crossing, direction).unwrap();
                let after = prepared.quote_exact_in(crossing + 1, direction).unwrap();
                assert!(before <= at && at <= after);
                let left_marginal = at - before;
                let right_marginal = after - at;
                let marginal_scale = left_marginal.max(right_marginal);
                assert!(
                    left_marginal.abs_diff(right_marginal) <= (marginal_scale / 1_000).max(2),
                    "center={center} direction={direction:?} stage={target_stage} crossing={crossing} left={left_marginal} right={right_marginal}"
                );

                let (_, split_base, split_quote, _) = raw_quote_endpoint(prepared, crossing - 1, direction);
                let split_tail = concentrated_prepare_curve(
                    split_base,
                    split_quote,
                    center,
                    PEAK_DEPTH_200,
                    FADE_TENTH,
                )
                .unwrap()
                .quote_exact_in(1, direction)
                .unwrap_or(0);
                assert!(
                    before + split_tail <= at,
                    "center={center} direction={direction:?} stage={target_stage} one_shot={at} split={before}+{split_tail}"
                );
            }
        }
    }
}

#[test]
fn full_precision_transition_removes_max_reserve_quote_staircases_and_stored_slack() {
    for (peak, fade, base, quote, adjacent_input) in [
        (
            100 * NAD as u128,
            100_000_000_u128,
            18_446_744_073_708_551_613_u128,
            5_671_181_913_068_248_712_u128,
            114_643_u128,
        ),
        (
            100 * NAD as u128,
            100_000_000_u128,
            999_999_999_999_999_999_u128,
            307_435_387_535_471_386_u128,
            12_429_u128,
        ),
        (
            2_000 * NAD as u128,
            1_000_000_u128,
            18_446_744_073_708_551_614_u128,
            16_603_345_133_192_447_584_u128,
            104_074_u128,
        ),
    ] {
        let quote_for = |amount_in| {
            concentrated_quote_exact_in(
                base,
                quote,
                amount_in,
                ConcentratedSwapDirection::BaseToQuote,
                NAD as u128,
                peak,
                fade,
            )
            .unwrap()
        };
        let one = quote_for(1);
        let twenty = quote_for(20);
        assert!(one <= 1, "one raw input atom harvested {one} output atoms");
        assert!(twenty <= 20, "twenty raw input atoms harvested {twenty} output atoms");

        let before = quote_for(adjacent_input);
        let after = quote_for(adjacent_input + 1);
        assert!(before <= after);
        assert!(after - before <= 1, "adjacent staircase remained: {before}->{after}");

        let mut sequential_base = base;
        let mut sequential_quote = quote;
        let mut sequential_output = 0_u128;
        for _ in 0..20 {
            let output = concentrated_quote_exact_in(
                sequential_base,
                sequential_quote,
                1,
                ConcentratedSwapDirection::BaseToQuote,
                NAD as u128,
                peak,
                fade,
            )
            .unwrap();
            if output == 0 {
                break;
            }
            sequential_base += 1;
            sequential_quote -= output;
            sequential_output += output;
        }
        assert!(sequential_output <= twenty);
    }
}

#[test]
fn full_precision_transition_residual_has_one_local_sign_crossing() {
    let peak = 200 * NAD as u128;
    let fade = FADE_TENTH;
    let geometry = Some(ConcentratedC1Geometry::derive(peak, fade).unwrap());
    let fixed = 1_377_336_837_576_107_u128;
    let invariant = 2_377_082_899_267_726_u128;
    let mut observed_valid = false;
    let mut crossings = 0_u8;
    for variable in 999_984_150_924_250_u128..=999_984_150_924_400_u128 {
        let valid = hybrid_residual(fixed, variable, invariant, geometry).unwrap().0;
        if valid && !observed_valid {
            crossings += 1;
            observed_valid = true;
        }
        assert!(!observed_valid || valid, "residual returned to invalid at y={variable}");
    }
    assert_eq!(crossings, 1);
}

#[test]
fn convergence_join_round_trips_cannot_create_raw_token_profit() {
    for (peak_depth_nad, fade_scale_nad) in [
        (2 * NAD as u128, NAD as u128 / 1_000),
        (PEAK_DEPTH_200, FADE_TENTH),
        (CONCENTRATED_MAX_PEAK_DEPTH_NAD, CONCENTRATED_MAX_FADE_SCALE_NAD),
    ] {
        let d = 2_000_000_000_000_u128;
        let geometry = ConcentratedC1Geometry::derive(peak_depth_nad, fade_scale_nad).unwrap();
        for (q_q48, v_q48) in [
            (geometry.q_start_q48, geometry.v_start_q48),
            (geometry.q_tail_q48, geometry.v_tail_q48),
        ] {
            let (low_common, high_common) = reserves_at_c1_coordinate(d, q_q48, v_q48);
            for (base, quote, direction) in [
                (high_common, low_common, ConcentratedSwapDirection::BaseToQuote),
                (low_common, high_common, ConcentratedSwapDirection::QuoteToBase),
            ] {
                for amount_in in [d / 1_000_000, d / 100_000, d / 10_000] {
                    let output = concentrated_quote_exact_in(
                        base,
                        quote,
                        amount_in,
                        direction,
                        NAD as u128,
                        peak_depth_nad,
                        fade_scale_nad,
                    )
                    .unwrap();
                    let (base_after, quote_after, reverse) = match direction {
                        ConcentratedSwapDirection::BaseToQuote => (
                            base + amount_in,
                            quote - output,
                            ConcentratedSwapDirection::QuoteToBase,
                        ),
                        ConcentratedSwapDirection::QuoteToBase => (
                            base - output,
                            quote + amount_in,
                            ConcentratedSwapDirection::BaseToQuote,
                        ),
                    };
                    let returned = concentrated_quote_exact_in(
                        base_after,
                        quote_after,
                        output,
                        reverse,
                        NAD as u128,
                        peak_depth_nad,
                        fade_scale_nad,
                    )
                    .unwrap();
                    assert!(
                        returned <= amount_in,
                        "round trip profited at peak={peak_depth_nad} fade={fade_scale_nad}: in={amount_in} back={returned}"
                    );
                }
            }
        }
    }
}

#[test]
fn concentrated_depth_improves_a_centered_quote() {
    let reserve = 1_000_000_000_000_000_u128;
    let input = 10_000_000_000_000_u128;
    let cpmm = calculate_normalized_amount_out(reserve, reserve, input).unwrap();
    let concentrated = concentrated_quote_exact_in(
        reserve,
        reserve,
        input,
        ConcentratedSwapDirection::BaseToQuote,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    assert!(concentrated > cpmm);
    assert!(concentrated < input);
}

#[test]
fn exact_out_replays_to_at_least_the_request() {
    let base = 1_000_000_000_000_000_u128;
    let quote = 1_100_000_000_000_000_u128;
    for requested in [1_000_000_u128, 1_000_000_000, 100_000_000_000] {
        let input = concentrated_quote_exact_out(
            base,
            quote,
            requested,
            ConcentratedSwapDirection::BaseToQuote,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        )
        .unwrap();
        let replay = concentrated_quote_exact_in(
            base,
            quote,
            input,
            ConcentratedSwapDirection::BaseToQuote,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        )
        .unwrap();
        assert!(
            replay >= requested,
            "requested={requested} input={input} replay={replay}"
        );
    }
}

#[test]
fn exact_out_replays_large_withdrawals_across_both_directions_and_shape_bounds() {
    let base = 1_000_000_000_000_u128;
    let quote = 1_000_000_000_000_u128;
    for (peak_depth_nad, fade_scale_nad) in [
        (2 * NAD as u128, NAD as u128 / 1_000),
        (PEAK_DEPTH_200, FADE_TENTH),
        (CONCENTRATED_MAX_PEAK_DEPTH_NAD, CONCENTRATED_MAX_FADE_SCALE_NAD),
    ] {
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            for requested in [quote / 100, quote / 2, quote * 4 / 5, quote * 19 / 20] {
                let input = concentrated_quote_exact_out(
                    base,
                    quote,
                    requested,
                    direction,
                    NAD as u128,
                    peak_depth_nad,
                    fade_scale_nad,
                )
                .unwrap();
                let replay = concentrated_quote_exact_in(
                    base,
                    quote,
                    input,
                    direction,
                    NAD as u128,
                    peak_depth_nad,
                    fade_scale_nad,
                )
                .unwrap();
                assert!(
                    replay >= requested,
                    "peak={peak_depth_nad} fade={fade_scale_nad} direction={direction:?} requested={requested} input={input} replay={replay}"
                );
            }
        }
    }
}

#[test]
fn exact_out_lower_bound_brackets_executable_input() {
    let base = 1_000_000_000_000_000_u128;
    let quote = 900_000_000_000_000_u128;
    let requested = 5_000_000_000_u128;
    let lower = concentrated_quote_exact_out_input_lower_bound(
        base,
        quote,
        requested,
        ConcentratedSwapDirection::BaseToQuote,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    let upper = concentrated_quote_exact_out(
        base,
        quote,
        requested,
        ConcentratedSwapDirection::BaseToQuote,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    assert!(lower <= upper);
}

#[test]
fn warm_and_cold_preparation_are_identical() {
    let start = concentrated_prepare_curve(
        1_000_000_000_000,
        1_200_000_000_000,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    let cold = concentrated_prepare_curve(
        1_010_000_000_000,
        1_190_000_000_000,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    let warm = concentrated_prepare_curve_seeded_cached(
        1_010_000_000_000,
        1_190_000_000_000,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
        start.geometry_cache().unwrap(),
        ConcentratedInvariantSeed::Hint(start.invariant_d()),
    )
    .unwrap();
    assert_eq!(warm.invariant_d(), cold.invariant_d());
}

#[test]
fn cached_geometry_reconstructs_authoritative_q80_geometry_exactly() {
    for (peak, fade) in [
        (2 * NAD as u128, 100_u128),
        (PEAK_DEPTH_200, FADE_TENTH),
        (CONCENTRATED_MAX_PEAK_DEPTH_NAD, CONCENTRATED_MAX_FADE_SCALE_NAD),
    ] {
        let cache = ConcentratedGeometryCache::derive(peak, fade).unwrap();
        assert!(cache.matches(peak, fade));
        assert_eq!(cache.peak_q80, mul_div_floor(peak, Q80_ONE, NAD as u128).unwrap());
        assert_eq!(cache.scale_q80, mul_div_floor(fade, Q80_ONE, NAD as u128).unwrap());
        assert_eq!(
            ConcentratedC1Geometry::from_cache(cache, peak, fade).unwrap(),
            ConcentratedC1Geometry::derive(peak, fade).unwrap()
        );
        let cached = concentrated_prepare_curve_cached(
            1_000_000_000_000,
            1_200_000_000_000,
            NAD as u128,
            peak,
            fade,
            cache,
        )
        .unwrap();
        let cold = concentrated_prepare_curve(
            1_000_000_000_000,
            1_200_000_000_000,
            NAD as u128,
            peak,
            fade,
        )
        .unwrap();
        assert_eq!(cached, cold);
        assert_eq!(cached.geometry_cache(), Some(cache));

        let mut inconsistent = cache;
        inconsistent.v_tail_q80 = inconsistent.v_start_q80;
        assert!(ConcentratedC1Geometry::from_cache(inconsistent, peak, fade).is_err());
        let mut stale = cache;
        stale.math_revision = stale.math_revision.wrapping_add(1);
        assert!(ConcentratedC1Geometry::from_cache(stale, peak, fade).is_err());
    }
}

#[test]
fn cached_ordinary_inner_quote_executes_without_q80_work_or_fallback() {
    let reserve = 1_000_000_000_000_000_u128;
    let cache = ConcentratedGeometryCache::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap();
    reset_sqrt_q80_evaluations();
    reset_q80_fallback_evaluations();
    let prepared = concentrated_prepare_curve_seeded_cached(
        reserve,
        reserve,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
        cache,
        ConcentratedInvariantSeed::Hint(reserve * 2),
    )
    .unwrap();
    let output = prepared
        .quote_exact_in(reserve / 1_000, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    assert!(output > 0);
    assert_eq!(sqrt_q80_evaluations(), 0);
    assert_eq!(q80_fallback_evaluations(), 0);
}

#[test]
fn restored_checkpoint_rejects_a_noncanonical_invariant() {
    let prepared = concentrated_prepare_curve(
        1_000_000_000_000,
        1_200_000_000_000,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    let invariant_d = prepared.invariant_d();
    let cache = prepared.geometry_cache().unwrap();
    assert!(concentrated_prepare_curve_seeded_cached(
        1_000_000_000_000,
        1_200_000_000_000,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
        cache,
        ConcentratedInvariantSeed::Exact(invariant_d),
    )
    .is_ok());
    assert!(concentrated_prepare_curve_seeded_cached(
        1_000_000_000_000,
        1_200_000_000_000,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
        cache,
        ConcentratedInvariantSeed::Exact(invariant_d.saturating_sub(2)),
    )
    .is_err());
}

#[test]
fn parameter_encoding_is_canonical() {
    let reserve = 1_000_000_000_000_u128;
    assert!(concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, 0,).is_err());
    assert!(concentrated_prepare_curve(reserve, reserve, NAD as u128, 0, FADE_TENTH,).is_err());
    assert!(concentrated_prepare_curve(
        reserve,
        reserve,
        NAD as u128,
        CONCENTRATED_MAX_PEAK_DEPTH_NAD + 1,
        FADE_TENTH,
    )
    .is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn wide_geometric_mean_is_the_exact_floor(
        x_raw in 1_u64..=u64::MAX,
        y_raw in 1_u64..=u64::MAX,
    ) {
        let x = x_raw as u128 * NAD as u128;
        let y = y_raw as u128 * NAD as u128;
        let root = geometric_mean_floor(x, y).unwrap();
        let product = U256::from(x) * U256::from(y);
        let square = U256::from(root) * U256::from(root);
        let successor = U256::from(root + 1) * U256::from(root + 1);

        prop_assert!(square <= product);
        prop_assert!(successor > product);
    }

    #[test]
    fn concentrated_balanced_q_matches_wide_integer_ordering(
        invariant_d in 1_u128..=(2_u128 * u64::MAX as u128),
        center_price_nad in 1_u128..=u64::MAX as u128,
    ) {
        let prepared = ConcentratedPreparedCurve {
            base_reserve_nad: NAD as u128,
            quote_reserve_nad: NAD as u128,
            base_common: NAD as u128,
            quote_common: NAD as u128,
            center_price_nad,
            peak_depth_nad: NAD as u128,
            fade_scale_nad: NAD as u128 / 10,
            invariant_d,
            common_numeraire: ConcentratedCommonNumeraire::for_center(center_price_nad).unwrap(),
            geometry: Some(ConcentratedC1Geometry::derive(NAD as u128, NAD as u128 / 10).unwrap()),
        };
        let q = prepared.balanced_equivalent_q().unwrap();
        let (ratio_numerator, ratio_denominator) = if center_price_nad >= NAD as u128 {
            (NAD as u128, center_price_nad * 4)
        } else {
            (center_price_nad, 4 * NAD as u128)
        };
        let denominator = U256::from(ratio_denominator);
        let radicand_numerator = U256::from(invariant_d)
            * U256::from(invariant_d)
            * U256::from(ratio_numerator);
        let square = U256::from(q) * U256::from(q);
        let successor = U256::from(q + 1) * U256::from(q + 1);

        prop_assert!(square * denominator <= radicand_numerator);
        prop_assert!(successor * denominator > radicand_numerator);
    }

    #[test]
    fn quotes_are_positive_bounded_and_monotone(
        base in 1_000_000_000_000_u128..1_000_000_000_000_000_u128,
        quote in 1_000_000_000_000_u128..1_000_000_000_000_000_u128,
        input in 1_000_000_u128..1_000_000_000_u128,
    ) {
        let first = concentrated_quote_exact_in(
            base,
            quote,
            input,
            ConcentratedSwapDirection::BaseToQuote,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        ).unwrap();
        let second = concentrated_quote_exact_in(
            base,
            quote,
            input * 2,
            ConcentratedSwapDirection::BaseToQuote,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        ).unwrap();
        prop_assert!(first <= second);
        prop_assert!(second < quote);
    }
}
