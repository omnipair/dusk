use super::*;
use crate::math::{denormalize_from_nad_ceil, normalize_to_nad};
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

struct InnerNewtonTestGuard(bool);

impl Drop for InnerNewtonTestGuard {
    fn drop(&mut self) {
        set_inner_newton_acceleration_enabled(self.0);
    }
}

fn with_inner_newton_acceleration<T>(enabled: bool, run: impl FnOnce() -> T) -> T {
    let previous = set_inner_newton_acceleration_enabled(enabled);
    let _guard = InnerNewtonTestGuard(previous);
    run()
}

struct CertifiedQ48ResidualTestGuard(bool);

impl Drop for CertifiedQ48ResidualTestGuard {
    fn drop(&mut self) {
        set_certified_q48_residuals_enabled(self.0);
    }
}

fn with_certified_q48_residuals<T>(enabled: bool, run: impl FnOnce() -> T) -> T {
    let previous = set_certified_q48_residuals_enabled(enabled);
    let _guard = CertifiedQ48ResidualTestGuard(previous);
    run()
}

fn assert_inner_newton_quote_differential(
    prepared: ConcentratedPreparedCurve,
    amount_in: u128,
    direction: ConcentratedSwapDirection,
) {
    reset_residual_evaluations();
    let baseline_output =
        with_inner_newton_acceleration(false, || prepared.quote_exact_in(amount_in, direction)).unwrap();
    let baseline_exact_in_evaluations = residual_evaluations();

    reset_residual_evaluations();
    let accelerated_output =
        with_inner_newton_acceleration(true, || prepared.quote_exact_in(amount_in, direction)).unwrap();
    let accelerated_exact_in_evaluations = residual_evaluations();
    assert_eq!(
        accelerated_output, baseline_output,
        "exact-in changed for direction={direction:?} amount={amount_in}"
    );
    assert!(
        accelerated_exact_in_evaluations <= CONCENTRATED_RESERVE_MAX_ITERS + 4,
        "accelerated exact-in exceeded its fixed proof budget: {accelerated_exact_in_evaluations}; baseline={baseline_exact_in_evaluations}"
    );

    if baseline_output > 1 {
        for amount_out in [1, baseline_output / 2, baseline_output] {
            if amount_out == 0 {
                continue;
            }
            reset_residual_evaluations();
            let baseline_bracket =
                with_inner_newton_acceleration(false, || prepared.quote_exact_out_input_bracket(amount_out, direction))
                    .unwrap();
            let baseline_exact_out_evaluations = residual_evaluations();

            reset_residual_evaluations();
            let accelerated_bracket =
                with_inner_newton_acceleration(true, || prepared.quote_exact_out_input_bracket(amount_out, direction))
                    .unwrap();
            let accelerated_exact_out_evaluations = residual_evaluations();
            assert_eq!(
                accelerated_bracket, baseline_bracket,
                "exact-out bracket changed for direction={direction:?} amount_out={amount_out}"
            );
            assert_eq!(accelerated_bracket.1 - accelerated_bracket.0, 1);
            assert!(
                accelerated_exact_out_evaluations <= CONCENTRATED_RESERVE_MAX_ITERS + 4,
                "accelerated exact-out exceeded its fixed proof budget: {accelerated_exact_out_evaluations}; baseline={baseline_exact_out_evaluations}"
            );
        }
    }
}

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
    let coefficient = mul_q80(
        2 * peak_q80,
        mul_q80(q_q80, mul_q80(weight_base, weight_base).unwrap()).unwrap(),
    )
    .unwrap();
    mul_q80(coefficient, h).unwrap() + q_q80 >= Q80_ONE
}

fn solve_inner_q_at_v_q80(v_q80: u128, q_floor: u128, peak_depth_nad: u128, fade_scale_nad: u128) -> u128 {
    let mut low = q_floor;
    let mut high = Q80_ONE;
    assert!(!inner_shape_residual_q80(low, v_q80, peak_depth_nad, fade_scale_nad));
    assert!(inner_shape_residual_q80(high, v_q80, peak_depth_nad, fade_scale_nad));
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
    let branch = prepared_branch_at_raw_reserves(prepared, base_after, quote_after).unwrap();
    (output, base_after, quote_after, branch)
}

fn prepared_branch_at_raw_reserves(
    prepared: ConcentratedPreparedCurve,
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
) -> Result<ConcentratedHybridBranch> {
    if prepared.peak_depth_nad == 0 {
        validate_common_reserves(base_reserve_nad, quote_reserve_nad)?;
        return Ok(ConcentratedHybridBranch::Inner);
    }
    let (base_common, quote_common) =
        normalize_reserves(base_reserve_nad, quote_reserve_nad, prepared.center_price_nad)?;
    prepared
        .geometry
        .ok_or(ErrorCode::BrokenInvariant)?
        .branch(base_common, quote_common)
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
    if prepared.peak_depth_nad == 0 {
        return;
    }
    let (canonical_sign, canonical_magnitude) =
        hybrid_residual(prepared.base_common, prepared.quote_common, high, prepared.geometry).unwrap();
    if canonical_sign {
        assert_eq!(canonical_magnitude, 0);
    } else {
        let adjacent = high.checked_sub(1).unwrap();
        assert!(
            hybrid_residual(prepared.base_common, prepared.quote_common, adjacent, prepared.geometry,)
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
        cpmm_amount_out_nad(x, y, dx).unwrap()
    );
    assert_eq!(
        concentrated_quote_exact_out(x, y, dy, ConcentratedSwapDirection::BaseToQuote, NAD as u128, 0, 0,).unwrap(),
        cpmm_amount_in_nad(x, y, dy).unwrap()
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
        let radicand_numerator = U256::from(invariant_d) * U256::from(invariant_d) * U256::from(ratio_numerator);
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
        let split_state = concentrated_prepare_curve(base_before, quote - before, NAD as u128, peak, fade).unwrap();
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
    let second_curve = concentrated_prepare_curve(base + first_input, quote - first, NAD as u128, peak, fade).unwrap();
    let second = second_curve
        .quote_exact_in(input - first_input, ConcentratedSwapDirection::BaseToQuote)
        .unwrap_or(0);
    assert_eq!((first, second), (49_210, 1));
    assert!(first + second <= one_shot, "one_shot={one_shot} split={first}+{second}");

    let reverse_curve = concentrated_prepare_curve(base + input, quote - one_shot, NAD as u128, peak, fade).unwrap();
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
        prepared_branch_at_raw_reserves(prepared, base, quote).unwrap(),
        ConcentratedHybridBranch::BaseScarceTail
    );
    assert_eq!(
        prepared
            .quote_exact_in(input, ConcentratedSwapDirection::BaseToQuote)
            .unwrap(),
        cpmm_amount_out_nad(base, quote, input).unwrap()
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
    let inputs = [1_u128, 2, 3, 15, 255, 4_095, 65_535, 131_071, 196_607, 262_143];
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
            let input =
                concentrated_quote_exact_out(base, quote, requested, direction, center, PEAK_DEPTH_200, FADE_TENTH)
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
    for center in [1_u128, NAD as u128 - 1, NAD as u128, NAD as u128 + 1, u64::MAX as u128] {
        let numeraire = ConcentratedCommonNumeraire::for_center(center).unwrap();
        for scale in [
            numeraire.base_scale(center).unwrap(),
            numeraire.quote_scale(center).unwrap(),
        ] {
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
    for (center_index, center) in [NAD as u128 - 1, NAD as u128, NAD as u128 + 1].into_iter().enumerate() {
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
fn supply_scaled_guidance_keeps_an_exact_radial_invariant_unclamped() {
    let reserve = NAD as u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let anchor = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();
    let successor_reserve = reserve * 101 / 100;
    let scaled_invariant_d = mul_div_u128(anchor.invariant_d(), 101, 100).unwrap();

    let successor = anchor
        .prepare_supply_scaled_guidance_successor(successor_reserve, successor_reserve, 100, 101)
        .unwrap();

    assert_eq!(scaled_invariant_d, successor_reserve * 2);
    assert_eq!(successor.invariant_d(), scaled_invariant_d);
}

#[test]
fn supply_scaled_guidance_clamps_a_one_atom_radial_under_round() {
    let reserve = NAD as u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let anchor = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();
    let base_successor = reserve * 101 / 100;
    let quote_successor = base_successor + 1;
    let scaled_invariant_d = mul_div_u128(anchor.invariant_d(), 101, 100).unwrap();

    let successor = anchor
        .prepare_supply_scaled_guidance_successor(base_successor, quote_successor, 100, 101)
        .unwrap();

    assert_eq!(successor.invariant_d(), scaled_invariant_d + 1);
    assert_eq!(successor.invariant_d(), base_successor + quote_successor);
}

#[test]
fn guidance_d_enclosure_actions_cover_old_valid_low_and_high_at_all_token_decimals() {
    for decimals in [0_u8, 6, 9] {
        let atom_nad = 10_u128.pow(u32::from(9 - decimals));
        let reserve = 100_u128.checked_mul(NAD as u128).unwrap();
        assert_eq!(reserve % atom_nad, 0, "decimals={decimals}");

        // An asymmetric canonical curve is strictly inside its radial/sum
        // enclosure. Re-preparing it 1:1 must remain byte-identical.
        let old_valid = concentrated_prepare_curve(
            reserve,
            reserve * 4 / 5,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        )
        .unwrap();
        let old_guidance = old_valid
            .prepare_guidance_successor_with_invariant(
                old_valid.base_reserve_nad(),
                old_valid.quote_reserve_nad(),
                old_valid.invariant_d(),
            )
            .unwrap();
        let (unchanged, unchanged_action) = old_guidance
            .prepare_supply_scaled_guidance_successor_with_action(
                old_valid.base_reserve_nad(),
                old_valid.quote_reserve_nad(),
                100,
                100,
            )
            .unwrap();
        assert_eq!(unchanged, old_guidance, "decimals={decimals}");
        assert_eq!(unchanged_action, ConcentratedGuidanceDAction::Unchanged);

        let balanced = concentrated_prepare_curve(
            reserve,
            reserve,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        )
        .unwrap();
        let balanced_guidance = balanced
            .prepare_guidance_successor_with_invariant(reserve, reserve, balanced.invariant_d())
            .unwrap();
        let (raised, raised_action) = balanced_guidance
            .prepare_supply_scaled_guidance_successor_with_action(reserve, reserve, 101, 100)
            .unwrap();
        assert_eq!(raised.invariant_d(), reserve * 2, "decimals={decimals}");
        assert_eq!(raised_action, ConcentratedGuidanceDAction::RaisedRadial);

        let (lowered, lowered_action) = balanced_guidance
            .prepare_supply_scaled_guidance_successor_with_action(reserve, reserve, 100, 101)
            .unwrap();
        assert_eq!(lowered.invariant_d(), reserve * 2, "decimals={decimals}");
        assert_eq!(lowered_action, ConcentratedGuidanceDAction::LoweredSum);

        // Equality is a kink, not an interior unchanged sample. When radial
        // and sum coincide, radial precedence is deterministic.
        let (boundary, boundary_action) = balanced_guidance
            .prepare_supply_scaled_guidance_successor_with_action(reserve, reserve, 100, 100)
            .unwrap();
        assert_eq!(boundary.invariant_d(), reserve * 2);
        assert_eq!(boundary_action, ConcentratedGuidanceDAction::RaisedRadial);

        let (local_raised, local_raised_action) = balanced_guidance
            .prepare_locally_floored_guidance_successor_with_action(reserve + atom_nad, reserve)
            .unwrap();
        assert!(local_raised.invariant_d() > balanced_guidance.invariant_d());
        assert_eq!(local_raised_action, ConcentratedGuidanceDAction::RaisedRadial);
        let (local_lowered, local_lowered_action) = balanced_guidance
            .prepare_locally_floored_guidance_successor_with_action(reserve - atom_nad, reserve - atom_nad)
            .unwrap();
        assert_eq!(local_lowered.invariant_d(), (reserve - atom_nad) * 2);
        assert_eq!(local_lowered_action, ConcentratedGuidanceDAction::LoweredSum);
    }
}

#[test]
fn canonical_anchor_enclosure_keeps_an_exact_radial_scale_unclamped() {
    let reserve = NAD as u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let anchor = canonical.seal_canonical_guidance_anchor().unwrap();
    let successor_reserve = reserve * 101 / 100;
    let independently_scaled = mul_div_u128(canonical.invariant_d(), 101, 100).unwrap();

    let successor = anchor
        .prepare_candidate_guidance(successor_reserve, successor_reserve, 100, 101)
        .unwrap();

    assert_eq!(independently_scaled, successor_reserve * 2);
    assert_eq!(successor.invariant_d(), independently_scaled);
    assert_eq!(anchor.guidance().invariant_d(), canonical.invariant_d());
}

#[test]
fn canonical_anchor_enclosure_clamps_a_one_atom_radial_under_round() {
    let reserve = NAD as u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let anchor = canonical.seal_canonical_guidance_anchor().unwrap();
    let base_successor = reserve * 101 / 100;
    let quote_successor = base_successor + 1;
    let independently_scaled = mul_div_u128(canonical.invariant_d(), 101, 100).unwrap();

    let successor = anchor
        .prepare_candidate_guidance(base_successor, quote_successor, 100, 101)
        .unwrap();

    assert_eq!(successor.invariant_d(), independently_scaled + 1);
    assert_eq!(successor.invariant_d(), base_successor + quote_successor);
}

#[test]
fn retained_guidance_floors_from_the_final_one_sided_endpoint() {
    let reserve = NAD as u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let trade = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();

    let retained = trade
        .prepare_locally_floored_guidance_successor(reserve + 1, reserve)
        .unwrap();

    assert_eq!(trade.invariant_d(), reserve * 2);
    assert_eq!(retained.invariant_d(), trade.invariant_d() + 1);
    assert_eq!(retained.invariant_d(), (reserve + 1) + reserve);
}

#[test]
fn structural_gap_certifies_consecutive_output_atoms_at_all_decimals() {
    let scale = ConcentratedCommonScale {
        numerator: 3,
        denominator: 2,
    };

    for decimals in [0_u8, 6, 9] {
        let atom_nad = 10_u128.pow(u32::from(9 - decimals));
        let output_reserve_nad = atom_nad * 20;
        let r_hi = atom_nad * 10;
        let r_lo = r_hi - atom_nad;
        let structural_low_common = scale.to_common_floor(r_lo).unwrap() + 1;
        let structural_high_common = scale.to_common_floor(r_hi).unwrap() - 1;

        reset_residual_evaluations();
        let amount_out = bounded_exact_in_structural_gap_output(
            scale,
            atom_nad,
            output_reserve_nad,
            structural_low_common,
            structural_high_common,
        )
        .unwrap();

        assert_eq!(amount_out, Some(output_reserve_nad - r_hi), "decimals={decimals}");
        assert_eq!(residual_evaluations(), 0, "decimals={decimals}");
        assert!(scale.to_common_floor(r_lo).unwrap() < structural_low_common);
        assert!(scale.to_common_floor(r_hi).unwrap() > structural_high_common);
    }
}

#[test]
fn structural_gap_rejects_an_interval_atom_and_zero_output_boundary() {
    let scale = ConcentratedCommonScale {
        numerator: 1,
        denominator: 1,
    };

    // The predecessor is itself inside [L, U], so this is an ordinary
    // bracket interval rather than a consecutive-atom structural gap.
    assert_eq!(
        bounded_exact_in_structural_gap_output(scale, 10, 100, 40, 49).unwrap(),
        None
    );
    // The first atom above U is the current reserve. Emitting it would be a
    // zero-output quote, which must remain a guidance miss.
    assert_eq!(
        bounded_exact_in_structural_gap_output(scale, 10, 100, 91, 99).unwrap(),
        None
    );
}

#[test]
fn structural_gap_quote_is_zero_probe_and_token_replay_exact_at_all_decimals() {
    let base_reserve = 1_000_000_000_000_000_u128;
    let quote_reserve = base_reserve * 2;
    let center = 2 * NAD as u128;
    let canonical =
        concentrated_prepare_curve(base_reserve, quote_reserve, center, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(base_reserve, quote_reserve, canonical.invariant_d())
        .unwrap();

    for decimals in [0_u8, 6, 9] {
        let output_atom_nad = 10_u128.pow(u32::from(9 - decimals));
        reset_residual_evaluations();
        let quote = guidance
            .quote_bounded_exact_in_with_mode(
                output_atom_nad * 3,
                ConcentratedSwapDirection::QuoteToBase,
                output_atom_nad,
            )
            .unwrap();

        assert_eq!(quote.mode, ConcentratedGuidanceExactInMode::StructuralGap);
        assert_eq!(quote.amount_out_nad, output_atom_nad);
        assert_eq!(quote.amount_out_nad % output_atom_nad, 0);
        assert_eq!(
            base_reserve.checked_sub(quote.amount_out_nad).unwrap(),
            base_reserve - output_atom_nad
        );
        assert_eq!(residual_evaluations(), 0, "decimals={decimals}");
        assert!(
            quote.amount_out_nad
                <= guidance
                    .quote_exact_in(output_atom_nad * 3, ConcentratedSwapDirection::QuoteToBase)
                    .unwrap()
        );
    }

    reset_residual_evaluations();
    let zero_output = guidance
        .quote_bounded_exact_in_with_mode(1, ConcentratedSwapDirection::QuoteToBase, 1)
        .unwrap();
    assert_eq!(zero_output.mode, ConcentratedGuidanceExactInMode::Bracket);
    assert_eq!(zero_output.amount_out_nad, 0);
    assert_eq!(residual_evaluations(), 0);
}

#[test]
fn bounded_exact_in_lattice_fallbacks_are_strict_and_deterministic() {
    let scale = ConcentratedCommonScale {
        numerator: 1,
        denominator: 1,
    };
    let probe = |reserve_nad, positive, magnitude| ConcentratedBoundedExactInProbe {
        reserve_nad,
        reserve_common: reserve_nad,
        context: ConcentratedResidualContext {
            branch: ConcentratedHybridBranch::Inner,
            target_q64: Q64_ONE,
            transition_cosh_q64: 0,
            transition_negative_q_prime_q64: 0,
        },
        evaluation: ConcentratedGuidanceResidualEvaluation {
            positive,
            magnitude_hint: magnitude,
            q64_hint: Q64_ONE,
        },
    };
    let high = probe(100, true, 1);

    // Endpoint-duplicate and absent accelerators both fall back to the exact
    // interior atom midpoint, never to either bracket endpoint.
    assert_eq!(
        bounded_exact_in_interior_raw(
            scale,
            10,
            20,
            Some(20),
            high,
            Some(BoundedExactInInteriorSeed::Common(100)),
        )
        .unwrap(),
        Some(60)
    );
    assert_eq!(
        bounded_exact_in_interior_raw(scale, 10, 20, Some(20), high, None).unwrap(),
        Some(60)
    );

    // Overflow in the optional false-position accelerator is a seed miss; the
    // same deterministic midpoint remains available from the verified bracket.
    let overflowing_low = probe(20, false, u128::MAX);
    assert_eq!(bounded_exact_in_false_position(overflowing_low, high), None);
    assert_eq!(
        bounded_exact_in_interior_raw(
            scale,
            10,
            overflowing_low.reserve_common,
            Some(overflowing_low.reserve_nad),
            high,
            bounded_exact_in_false_position(overflowing_low, high),
        )
        .unwrap(),
        Some(60)
    );

    // A one-atom bracket has no strict lattice interior and must retain H.
    assert_eq!(
        bounded_exact_in_interior_raw(
            scale,
            10,
            90,
            Some(90),
            high,
            Some(BoundedExactInInteriorSeed::Common(95)),
        )
        .unwrap(),
        None
    );
}

#[test]
fn bounded_exact_in_zero_false_position_uses_first_strict_raw_atom_at_all_decimals() {
    let scale = ConcentratedCommonScale {
        numerator: 3,
        denominator: 2,
    };
    let probe = |reserve_nad, reserve_common, positive, magnitude| ConcentratedBoundedExactInProbe {
        reserve_nad,
        reserve_common,
        context: ConcentratedResidualContext {
            branch: ConcentratedHybridBranch::Inner,
            target_q64: Q64_ONE,
            transition_cosh_q64: 0,
            transition_negative_q_prime_q64: 0,
        },
        evaluation: ConcentratedGuidanceResidualEvaluation {
            positive,
            magnitude_hint: magnitude,
            q64_hint: Q64_ONE,
        },
    };

    for decimals in [0_u8, 6, 9] {
        let atom_nad = 10_u128.pow(u32::from(9 - decimals));
        let low_raw = atom_nad * 10;
        let high_raw = atom_nad * 14;
        let low_common = scale.to_common_floor(low_raw).unwrap();
        let high_common = scale.to_common_floor(high_raw).unwrap();
        let width = high_common - low_common;
        let low = probe(low_raw, low_common, false, 1);
        let high = probe(high_raw, high_common, true, width);
        let seed = bounded_exact_in_false_position(low, high);

        assert_eq!(seed, Some(BoundedExactInInteriorSeed::FirstRawInterior));
        assert_eq!(
            bounded_exact_in_interior_raw(scale, atom_nad, low_common, Some(low_raw), high, seed,).unwrap(),
            Some(low_raw + atom_nad),
            "decimals={decimals}"
        );

        let adjacent_high_raw = low_raw + atom_nad;
        let adjacent_high = probe(
            adjacent_high_raw,
            scale.to_common_floor(adjacent_high_raw).unwrap(),
            true,
            width,
        );
        assert_eq!(
            bounded_exact_in_interior_raw(
                scale,
                atom_nad,
                low_common,
                Some(low_raw),
                adjacent_high,
                Some(BoundedExactInInteriorSeed::FirstRawInterior),
            )
            .unwrap(),
            None,
            "decimals={decimals} must retain H when no strict raw atom exists"
        );
    }
}

#[test]
fn bounded_exact_in_p4_consumes_first_raw_atom_and_matches_canonical_preview() {
    let reserve = 1_000_000_000_000_000_u128;
    let amount_in = 1_000_000_000_000_u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();

    reset_residual_evaluations();
    reset_bounded_exact_in_first_raw_fallbacks();
    let exact_only_bounded = with_certified_q48_residuals(false, || {
        guidance
            .quote_bounded_exact_in_with_four_probes(amount_in, ConcentratedSwapDirection::BaseToQuote, 1)
            .unwrap()
            .amount_out_nad
    });
    let exact_only_probes = residual_evaluations();
    let (first_raw_probe_ordinal, low_raw, selected_raw) = bounded_exact_in_first_raw_trace();
    assert_eq!(bounded_exact_in_first_raw_fallbacks(), 1);
    assert_eq!(first_raw_probe_ordinal, 4);
    assert_eq!(selected_raw, low_raw + 1);
    assert_eq!(selected_raw, reserve - exact_only_bounded);
    assert_eq!(exact_only_probes, 4);

    reset_residual_evaluations();
    let exact_only_max5 = with_certified_q48_residuals(false, || {
        guidance
            .quote_bounded_exact_in_with_mode(amount_in, ConcentratedSwapDirection::BaseToQuote, 1)
            .unwrap()
            .amount_out_nad
    });
    assert!((1..=MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG).contains(&residual_evaluations()));

    reset_residual_evaluations();
    let bounded = guidance
        .quote_bounded_exact_in_with_mode(amount_in, ConcentratedSwapDirection::BaseToQuote, 1)
        .unwrap()
        .amount_out_nad;
    let probes = residual_evaluations();
    let exact = guidance
        .quote_exact_in(amount_in, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    assert_eq!(bounded, exact_only_max5);
    assert_eq!(reserve - bounded, selected_raw);
    assert!((1..=MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG).contains(&probes));
    assert_eq!(exact_only_bounded, 999_995_024_796);
    assert_eq!(exact_only_bounded, exact);
}

#[test]
fn bounded_exact_in_p5_closes_funding_quote_error_without_overquoting() {
    let base_reserve = 1_200_000_000_000_000_u128;
    let quote_reserve = 2_400_000_000_000_000_u128;
    let amount_in = 348_950_000_000_000_u128;
    let output_atom_nad = 1_000_u128;
    let canonical =
        concentrated_prepare_curve(base_reserve, quote_reserve, 2 * NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    assert_eq!(canonical.invariant_d(), 4_800_000_000_000_000);
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(base_reserve, quote_reserve, canonical.invariant_d())
        .unwrap();

    reset_residual_evaluations();
    let p4 = guidance
        .quote_bounded_exact_in_with_four_probes(amount_in, ConcentratedSwapDirection::QuoteToBase, output_atom_nad)
        .unwrap()
        .amount_out_nad;
    assert_eq!(residual_evaluations(), 4);

    reset_residual_evaluations();
    reset_certified_q48_residual_evaluations();
    reset_bounded_q48_reciprocal_counters();
    let p5 = guidance
        .quote_bounded_exact_in_with_mode(amount_in, ConcentratedSwapDirection::QuoteToBase, output_atom_nad)
        .unwrap();
    assert_eq!(residual_evaluations(), 5);
    assert_eq!(certified_q48_residual_evaluations(), 5);
    assert_eq!(certified_q48_exact_fallback_evaluations(), 0);
    assert_eq!(bounded_q48_reciprocal_counters(), (1, 10, 0));
    assert_eq!(p5.mode, ConcentratedGuidanceExactInMode::Bracket);

    let exact = guidance
        .quote_exact_in(amount_in, ConcentratedSwapDirection::QuoteToBase)
        .unwrap();
    assert_eq!(bounded_q48_reciprocal_counters(), (1, 10, 0));
    let exact_aligned = exact - exact % output_atom_nad;
    assert_eq!(p4, 174_278_993_105_000);
    assert_eq!(p5.amount_out_nad, 174_287_049_380_000);
    assert_eq!(exact_aligned, 174_287_050_458_000);
    assert_eq!((exact_aligned - p4) / output_atom_nad, 8_057_353);
    assert_eq!((exact_aligned - p5.amount_out_nad) / output_atom_nad, 1_078);
    assert_eq!(p4 % output_atom_nad, 0);
    assert_eq!(p5.amount_out_nad % output_atom_nad, 0);
    assert!(p5.amount_out_nad >= p4);
    assert!(p5.amount_out_nad <= exact_aligned);
}

fn concentrated_error_number(error: anchor_lang::error::Error) -> u64 {
    match error {
        anchor_lang::error::Error::AnchorError(error) => u64::from(error.error_code_number),
        anchor_lang::error::Error::ProgramError(error) => u64::from(error.program_error),
    }
}

#[test]
fn certified_q48_and_exact_only_have_identical_canonical_outputs_across_regions() {
    let geometry = ConcentratedC1Geometry::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let transition_v = geometry.v_start_q48 + (geometry.v_tail_q48 - geometry.v_start_q48) / 2;
    let (transition_q, _) = geometry.transition_q_and_slope_at_v(transition_v).unwrap();
    let transition = reserves_at_c1_coordinate(2_000_000_000_000_000, transition_q, transition_v);
    let fixtures = [
        (
            ConcentratedHybridBranch::Inner,
            1_000_000_000_000_000_u128,
            1_050_000_000_000_000_u128,
        ),
        (
            ConcentratedHybridBranch::BaseScarceTransition,
            transition.0,
            transition.1,
        ),
        (
            ConcentratedHybridBranch::BaseScarceTail,
            1_000_000_000_u128,
            8_000_000_000_000_u128,
        ),
    ];

    for (expected_branch, base, quote) in fixtures {
        let exact_only = with_certified_q48_residuals(false, || {
            concentrated_prepare_curve(base, quote, NAD as u128, PEAK_DEPTH_200, FADE_TENTH)
        })
        .unwrap();
        reset_certified_q48_residual_evaluations();
        let fast = concentrated_prepare_curve(base, quote, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
        assert_eq!(fast, exact_only, "prepare identity for {expected_branch:?}");
        assert_eq!(certified_q48_residual_evaluations(), 0);
        assert_eq!(certified_q48_exact_fallback_evaluations(), 0);
        assert_eq!(
            geometry.branch(fast.base_common, fast.quote_common).unwrap(),
            expected_branch
        );
        let exact_only_guidance = exact_only
            .prepare_guidance_successor_with_invariant(base, quote, exact_only.invariant_d())
            .unwrap();
        let fast_guidance = fast
            .prepare_guidance_successor_with_invariant(base, quote, fast.invariant_d())
            .unwrap();

        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            let input_reserve = match direction {
                ConcentratedSwapDirection::BaseToQuote => base,
                ConcentratedSwapDirection::QuoteToBase => quote,
            };
            let amount_in = (input_reserve / 1_000).max(1);
            let exact_only_output =
                with_certified_q48_residuals(false, || exact_only.quote_exact_in(amount_in, direction)).unwrap();
            let certified_before_exact_in = certified_q48_residual_evaluations();
            let fallback_before_exact_in = certified_q48_exact_fallback_evaluations();
            let fast_output = fast.quote_exact_in(amount_in, direction).unwrap();
            assert_eq!(
                fast_output, exact_only_output,
                "exact-in identity for {expected_branch:?} {direction:?}"
            );
            assert_eq!(certified_q48_residual_evaluations(), certified_before_exact_in);
            assert_eq!(certified_q48_exact_fallback_evaluations(), fallback_before_exact_in);

            let exact_only_bounded = with_certified_q48_residuals(false, || {
                exact_only_guidance.quote_bounded_exact_in(amount_in, direction, 1)
            });
            let fast_bounded = fast_guidance.quote_bounded_exact_in(amount_in, direction, 1);
            assert_eq!(
                fast_bounded, exact_only_bounded,
                "bounded identity for {expected_branch:?} {direction:?}"
            );
            if let Ok(output) = fast_bounded {
                assert!(output <= fast_output);
            }

            if fast_output > 0 {
                let amount_out = (fast_output / 2).max(1);
                let exact_only_bracket = with_certified_q48_residuals(false, || {
                    exact_only.quote_exact_out_input_bracket(amount_out, direction)
                })
                .unwrap();
                let certified_before_exact_out = certified_q48_residual_evaluations();
                let fallback_before_exact_out = certified_q48_exact_fallback_evaluations();
                let fast_bracket = fast.quote_exact_out_input_bracket(amount_out, direction).unwrap();
                assert_eq!(
                    fast_bracket, exact_only_bracket,
                    "exact-out identity for {expected_branch:?} {direction:?}"
                );
                assert_eq!(certified_q48_residual_evaluations(), certified_before_exact_out);
                assert_eq!(certified_q48_exact_fallback_evaluations(), fallback_before_exact_out);
            }
        }

        if expected_branch == ConcentratedHybridBranch::BaseScarceTransition {
            assert!(certified_q48_exact_fallback_evaluations() > 0);
        } else {
            assert!(certified_q48_residual_evaluations() > 0);
        }
    }

    let canonical = concentrated_prepare_curve(
        1_000_000_000_000_000,
        1_000_000_000_000_000,
        NAD as u128,
        PEAK_DEPTH_200,
        FADE_TENTH,
    )
    .unwrap();
    let invalid = canonical
        .prepare_guidance_successor_with_invariant(
            canonical.base_reserve_nad,
            canonical.quote_reserve_nad,
            canonical.invariant_d() / 2,
        )
        .unwrap();
    let exact_only_error = with_certified_q48_residuals(false, || {
        invalid
            .quote_bounded_exact_in(1_000_000_000, ConcentratedSwapDirection::BaseToQuote, 1)
            .unwrap_err()
    });
    let fast_error = invalid
        .quote_bounded_exact_in(1_000_000_000, ConcentratedSwapDirection::BaseToQuote, 1)
        .unwrap_err();
    assert_eq!(
        concentrated_error_number(fast_error),
        concentrated_error_number(exact_only_error)
    );
}

#[test]
fn certified_q48_bounded_quotes_match_exact_only_at_zero_six_and_nine_decimals() {
    let reserve = 1_000_000_000_000_000_u128;
    let amount_in = reserve * 35 / 100;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();

    for decimals in [0_u8, 6, 9] {
        let output_atom_nad = 10_u128.pow(u32::from(9 - decimals));
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            let exact_only = with_certified_q48_residuals(false, || {
                guidance.quote_bounded_exact_in(amount_in, direction, output_atom_nad)
            })
            .unwrap();
            let fast = guidance
                .quote_bounded_exact_in(amount_in, direction, output_atom_nad)
                .unwrap();
            let exact = guidance.quote_exact_in(amount_in, direction).unwrap();
            assert_eq!(fast, exact_only, "decimals={decimals} direction={direction:?}");
            assert_eq!(fast % output_atom_nad, 0);
            assert!(fast <= exact);
        }
    }
}

fn assert_bounded_q48_reciprocal_exact(numerator: u128, denominator: u128) {
    let reciprocal = BoundedQ48Reciprocal::new(denominator).unwrap();
    let (actual, actual_remainder) = reciprocal.exact_floor_rem(numerator, denominator).unwrap();
    let scaled = numerator * Q48_ONE;
    assert_eq!(actual, scaled / denominator, "n={numerator} d={denominator}");
    assert_eq!(actual_remainder, scaled % denominator, "n={numerator} d={denominator}");
}

#[test]
fn bounded_q48_reciprocal_is_exact_at_domain_edges_and_two_corrections() {
    let maximum = MAX_COMMON_RESERVE * 2;
    let boundaries = [
        0,
        1,
        2,
        3,
        (1_u128 << 32) - 1,
        1_u128 << 32,
        (1_u128 << 48) - 1,
        1_u128 << 48,
        (1_u128 << 63) - 1,
        1_u128 << 63,
        MAX_COMMON_RESERVE,
        maximum,
    ];
    for denominator in boundaries.into_iter().skip(1) {
        for numerator in boundaries {
            assert_bounded_q48_reciprocal_exact(numerator, denominator);
        }
    }

    // Locked witness for the proven worst-case underestimate of two.
    let numerator = 28_492_241_963_541_219_001_u128;
    let denominator = 29_716_711_854_020_950_695_u128;
    let reciprocal = BoundedQ48Reciprocal::new(denominator).unwrap();
    let exact = (numerator * Q48_ONE) / denominator;
    assert_eq!(exact - reciprocal.approximate_floor(numerator).unwrap(), 2);
    assert_eq!(reciprocal.exact_floor_rem(numerator, denominator).unwrap().0, exact);

    assert!(BoundedQ48Reciprocal::new(maximum + 1).is_none());
}

#[test]
fn bounded_q48_reciprocal_matches_exact_division_on_deterministic_sweep() {
    let maximum = MAX_COMMON_RESERVE * 2;
    let mut state = 0x9e37_79b9_7f4a_7c15_d1b5_4a32_d192_ed03_u128;
    for _ in 0..20_000 {
        state = state
            .wrapping_mul(0x5851_f42d_4c95_7f2d_1405_7b7e_f767_814f_u128)
            .wrapping_add(0x1405_7b7e_f767_814f_5851_f42d_4c95_7f2d_u128);
        let denominator = state % maximum + 1;
        state = state
            .wrapping_mul(0x5851_f42d_4c95_7f2d_1405_7b7e_f767_814f_u128)
            .wrapping_add(0x1405_7b7e_f767_814f_5851_f42d_4c95_7f2d_u128);
        let numerator = state % (maximum + 1);
        assert_bounded_q48_reciprocal_exact(numerator, denominator);
    }
}

#[test]
fn bounded_q48_reciprocal_mismatch_and_domain_miss_use_exact_division() {
    let maximum = MAX_COMMON_RESERVE * 2;
    let reciprocal = BoundedQ48Reciprocal::new(7).unwrap();
    reset_bounded_q48_reciprocal_counters();
    let mismatch = ratio_q48_interval(11, 5, Some(&reciprocal)).unwrap();
    let domain_miss = ratio_q48_interval(maximum + 1, 7, Some(&reciprocal)).unwrap();
    assert_eq!(mismatch.lower, 11 * Q48_ONE / 5);
    assert_eq!(mismatch.upper, mismatch.lower + 1);
    assert_eq!(domain_miss.lower, (maximum + 1) * Q48_ONE / 7);
    assert_eq!(domain_miss.upper, domain_miss.lower + 1);
    assert_eq!(bounded_q48_reciprocal_counters(), (0, 0, 2));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn certified_q48_bounded_success_is_atom_aligned_and_same_d_conservative(
        base_units in 10_000_u128..1_000_000_u128,
        quote_ratio_bps in 8_000_u128..12_001_u128,
        input_bps in 1_u128..3_501_u128,
        decimals in prop_oneof![Just(0_u8), Just(6_u8), Just(9_u8)],
        base_to_quote in any::<bool>(),
    ) {
        let base = base_units * NAD as u128;
        let quote = (base_units * quote_ratio_bps / 10_000) * NAD as u128;
        let canonical_exact_only = with_certified_q48_residuals(false, || {
            concentrated_prepare_curve(base, quote, NAD as u128, PEAK_DEPTH_200, FADE_TENTH)
        }).unwrap();
        let canonical = concentrated_prepare_curve(base, quote, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
        prop_assert_eq!(canonical, canonical_exact_only);
        let guidance = canonical
            .prepare_guidance_successor_with_invariant(base, quote, canonical.invariant_d())
            .unwrap();
        let direction = if base_to_quote {
            ConcentratedSwapDirection::BaseToQuote
        } else {
            ConcentratedSwapDirection::QuoteToBase
        };
        let input_reserve = if base_to_quote { base } else { quote };
        let amount_in = (input_reserve * input_bps / 10_000).max(1);
        let output_atom_nad = 10_u128.pow(u32::from(9 - decimals));
        if let Ok(fast) = guidance.quote_bounded_exact_in(amount_in, direction, output_atom_nad) {
            let exact = guidance.quote_exact_in(amount_in, direction).unwrap();
            prop_assert_eq!(fast % output_atom_nad, 0);
            prop_assert!(fast <= exact);
        }
    }
}

#[test]
fn bounded_same_d_guidance_is_conservative_in_both_directions_with_five_probes() {
    let fixtures = [
        (1_000_000_000_000_000_u128, 1_000_000_000_000_000_u128),
        (1_000_000_000_000, 1_350_000_000_000),
        (8_000_000_000_000, 1_000_000_000),
        (1_000_000_000, 8_000_000_000_000),
    ];
    let mut exact_in_successes = [0_u32; 2];
    let mut exact_out_successes = [0_u32; 2];
    let mut exact_out_p3_positive = 0_u32;

    for (base, quote) in fixtures {
        let canonical = concentrated_prepare_curve(base, quote, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
        let guidance = canonical
            .prepare_guidance_successor_with_invariant(base, quote, canonical.invariant_d())
            .unwrap();
        for (direction_index, direction) in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ]
        .into_iter()
        .enumerate()
        {
            let input_reserve = match direction {
                ConcentratedSwapDirection::BaseToQuote => base,
                ConcentratedSwapDirection::QuoteToBase => quote,
            };
            let output_reserve = match direction {
                ConcentratedSwapDirection::BaseToQuote => quote,
                ConcentratedSwapDirection::QuoteToBase => base,
            };
            let amount_in = (input_reserve / 1_000).max(1);

            reset_residual_evaluations();
            let bounded_in = guidance.quote_bounded_exact_in(amount_in, direction, 1);
            let exact_in_probes = residual_evaluations();
            assert!(
                exact_in_probes <= MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG,
                "exact-in probes={exact_in_probes} base={base} quote={quote} direction={direction:?}"
            );
            if let Ok(estimate) = bounded_in {
                let exact = guidance.quote_exact_in(amount_in, direction).unwrap();
                assert!(
                    estimate <= exact,
                    "bounded exact-in overquoted: estimate={estimate} exact={exact} base={base} quote={quote} direction={direction:?}"
                );
                if estimate > 0 {
                    exact_in_successes[direction_index] += 1;
                }
            }

            let amount_out = (output_reserve / 10_000).max(1);
            reset_residual_evaluations();
            let bounded_out = guidance.quote_bounded_exact_out_input(amount_out, direction, 1);
            let exact_out_probes = residual_evaluations();
            assert!(
                exact_out_probes <= MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG,
                "exact-out probes={exact_out_probes} base={base} quote={quote} direction={direction:?}"
            );
            if let Ok(estimate) = bounded_out {
                exact_out_p3_positive +=
                    u32::from(estimate.mode == ConcentratedGuidanceExactOutMode::BoundedP3Positive);
                let exact = guidance.quote_exact_out_input_bracket(amount_out, direction).unwrap().1;
                assert!(
                    estimate.amount_in_nad >= exact,
                    "bounded exact-out underquoted input: estimate={} exact={exact} base={base} quote={quote} direction={direction:?}",
                    estimate.amount_in_nad,
                );
                assert!(guidance.quote_exact_in(estimate.amount_in_nad, direction).unwrap() >= amount_out);
                assert!((2..=MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG).contains(&exact_out_probes));
                exact_out_successes[direction_index] += 1;
            }
        }
    }

    assert!(exact_in_successes.into_iter().all(|successes| successes > 0));
    assert!(exact_out_successes.into_iter().all(|successes| successes > 0));
    assert!(exact_out_p3_positive > 0);
}

#[test]
fn bounded_exact_out_max3_replays_on_zero_six_and_nine_decimal_input_lattices() {
    let reserve = 1_000_000_000_000_000_u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();
    let amount_out_nad = reserve / 10_000;

    for decimals in [0_u8, 6, 9] {
        let input_atom_nad = 10_u128.pow(u32::from(9 - decimals));
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            reset_residual_evaluations();
            let bounded = guidance
                .quote_bounded_exact_out_input(amount_out_nad, direction, input_atom_nad)
                .unwrap();
            let probes = residual_evaluations();
            assert!((2..=MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG).contains(&probes));
            assert_eq!(bounded.amount_in_nad % input_atom_nad, 0);
            let raw = denormalize_from_nad_ceil(bounded.amount_in_nad, decimals).unwrap();
            assert_eq!(normalize_to_nad(raw as u128, decimals).unwrap(), bounded.amount_in_nad);
            let exact = guidance
                .quote_exact_out_input_bracket(amount_out_nad, direction)
                .unwrap()
                .1;
            assert!(bounded.amount_in_nad >= bounded_exact_in_align_raw_up(exact, input_atom_nad).unwrap());
            assert!(guidance.quote_exact_in(bounded.amount_in_nad, direction).unwrap() >= amount_out_nad);
            if bounded.mode == ConcentratedGuidanceExactOutMode::BoundedP3Positive {
                assert_eq!(probes, MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG);
            }
        }
    }
}

#[test]
fn bounded_exact_out_without_strict_input_atom_interior_keeps_verified_p2_high() {
    let reserve = 1_000_000_000_000_000_u128;
    let input_atom_nad = NAD as u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();

    reset_residual_evaluations();
    reset_bounded_q48_reciprocal_counters();
    let bounded = guidance
        .quote_bounded_exact_out_input(1, ConcentratedSwapDirection::BaseToQuote, input_atom_nad)
        .unwrap();
    assert_eq!(residual_evaluations(), 2);
    let reciprocal_counts = bounded_q48_reciprocal_counters();
    assert_eq!(reciprocal_counts.0, 1);
    assert_eq!(reciprocal_counts.1 + reciprocal_counts.2, 4);
    assert_eq!(bounded.mode, ConcentratedGuidanceExactOutMode::BoundedP2High);
    assert_eq!(bounded.amount_in_nad, input_atom_nad);
}

#[test]
fn bounded_exact_out_cpmm_is_analytic_aligned_and_hint_fallback_stays_scalar() {
    let reserve = 1_000_000_000_000_000_u128;
    let input_atom_nad = NAD as u128;
    let canonical = concentrated_prepare_curve(reserve, reserve * 2, NAD as u128, 0, 0).unwrap();
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve * 2, canonical.invariant_d())
        .unwrap();
    let amount_out_nad = 10 * input_atom_nad;

    reset_residual_evaluations();
    let bounded = guidance
        .quote_bounded_exact_out_input(amount_out_nad, ConcentratedSwapDirection::BaseToQuote, input_atom_nad)
        .unwrap();
    assert_eq!(residual_evaluations(), 0);
    assert_eq!(bounded.mode, ConcentratedGuidanceExactOutMode::AnalyticCpmm);
    assert_eq!(bounded.amount_in_nad % input_atom_nad, 0);

    let successor_base = reserve + 17 * input_atom_nad;
    let successor_quote = reserve * 2 - 11 * input_atom_nad;
    let scalar = guidance
        .quote_hint_successor_exact_out_input_upper(
            successor_base,
            successor_quote,
            amount_out_nad,
            ConcentratedSwapDirection::BaseToQuote,
            input_atom_nad,
        )
        .unwrap();
    let exact = guidance
        .prepare_hint_successor(successor_base, successor_quote)
        .unwrap()
        .quote_exact_out_input_bracket(amount_out_nad, ConcentratedSwapDirection::BaseToQuote)
        .unwrap()
        .1;
    assert_eq!(scalar, bounded_exact_in_align_raw_up(exact, input_atom_nad).unwrap());
}

#[test]
fn bounded_exact_in_is_atom_aligned_and_conservative_at_zero_six_and_nine_decimals() {
    let reserve = 1_000_000_000_000_000_u128;
    let amount_in = reserve * 35 / 100;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();

    for decimals in [0_u8, 6, 9] {
        let output_atom_nad = 10_u128.pow(u32::from(9 - decimals));
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            reset_residual_evaluations();
            let bounded = guidance
                .quote_bounded_exact_in(amount_in, direction, output_atom_nad)
                .unwrap();
            let probes = residual_evaluations();
            let exact = guidance.quote_exact_in(amount_in, direction).unwrap();
            assert!(bounded > 0, "decimals={decimals} direction={direction:?}");
            assert_eq!(bounded % output_atom_nad, 0);
            assert!(
                bounded <= exact,
                "decimals={decimals} direction={direction:?} bounded={bounded} exact={exact}"
            );
            assert!(
                (1..=MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG).contains(&probes),
                "decimals={decimals} direction={direction:?} probes={probes}"
            );
        }
    }
}

#[test]
fn bounded_exact_in_crosses_outward_branches_without_exceeding_same_d_authority() {
    let reserve = 1_000_000_000_000_000_u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let guidance = canonical
        .prepare_guidance_successor_with_invariant(reserve, reserve, canonical.invariant_d())
        .unwrap();

    for direction in [
        ConcentratedSwapDirection::BaseToQuote,
        ConcentratedSwapDirection::QuoteToBase,
    ] {
        let amount_in = first_raw_input_at_outward_stage(canonical, direction, 2);
        reset_residual_evaluations();
        let bounded = guidance.quote_bounded_exact_in(amount_in, direction, 1).unwrap();
        let probes = residual_evaluations();
        let exact = guidance.quote_exact_in(amount_in, direction).unwrap();
        let (base_after, quote_after) = match direction {
            ConcentratedSwapDirection::BaseToQuote => (reserve + amount_in, reserve - bounded),
            ConcentratedSwapDirection::QuoteToBase => (reserve - bounded, reserve + amount_in),
        };
        let branch = prepared_branch_at_raw_reserves(canonical, base_after, quote_after).unwrap();

        assert!(bounded > 0);
        assert!(bounded <= exact);
        assert!(
            outward_branch_stage(branch, direction) >= 1,
            "direction={direction:?} branch={branch:?}"
        );
        assert!(
            (1..=MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG).contains(&probes),
            "direction={direction:?} probes={probes}"
        );
    }
}

#[test]
fn bounded_guidance_rejects_invalid_basis_but_allows_off_curve_probe_geometry() {
    let reserve = 1_000_000_000_000_000_u128;
    let canonical = concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();

    for invalid_d in [
        reserve.checked_mul(2).unwrap().checked_add(1).unwrap(),
        canonical.invariant_d() / 2,
    ] {
        let invalid = canonical
            .prepare_guidance_successor_with_invariant(reserve, reserve, invalid_d)
            .unwrap();
        reset_residual_evaluations();
        assert!(invalid
            .quote_bounded_exact_in(reserve / 1_000, ConcentratedSwapDirection::BaseToQuote, 1)
            .is_err());
        assert_eq!(residual_evaluations(), 0);
    }

    let geometry = canonical.geometry.unwrap();
    // Off-curve quote probes may legitimately sit on either side of x+y=D;
    // only raw reserve bounds and branch-specific minimums apply to them.
    assert!(bounded_guidance_residual_context(reserve * 2, reserve, geometry).is_ok());
    assert!(bounded_guidance_residual_context(reserve, reserve / 2, geometry).is_ok());
    assert!(
        bounded_guidance_residual_context(MIN_INNER_COMMON_RESERVE - 1, MIN_INNER_COMMON_RESERVE, geometry).is_err()
    );
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
fn ordinary_inner_quote_uses_bounded_newton_work_without_q80_fallback() {
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
        residual_evaluations() <= 8,
        "ordinary inner quote used {} residual probes",
        residual_evaluations()
    );
    assert_eq!(sqrt_q80_evaluations(), 0);
    assert_eq!(q80_fallback_evaluations(), 0);
}

#[test]
fn inner_newton_guidance_covers_both_sides_of_q_one() {
    let d = 2_000_000_000_u128;
    let geometry = ConcentratedC1Geometry::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap();
    for (fixed, variable, expected_q_above_one) in [
        (999_999_000_u128, 1_000_001_000_u128, false),
        (1_000_000_001, 1_000_000_001, true),
    ] {
        let context = ConcentratedResidualContext::derive(geometry, fixed, variable).unwrap();
        assert_eq!(context.branch, ConcentratedHybridBranch::Inner);
        let evaluation =
            hybrid_residual_evaluation_with_context(fixed, variable, d, Some(geometry), Some(context)).unwrap();
        assert_eq!(evaluation.q64 > Q64_ONE, expected_q_above_one);
        assert_ne!(evaluation.q64, Q64_ONE);
        let candidate = inner_newton_probe(fixed, variable, d, evaluation.accelerator(), geometry)
            .expect("q=1-adjacent inner state must produce Newton guidance");
        if evaluation.positive {
            assert!(candidate < variable);
        } else {
            assert!(candidate > variable);
        }
    }
}

#[test]
fn inner_newton_matches_prior_search_at_center_and_domain_extremes() {
    for (base, quote, peak, fade, amounts) in [
        (
            1_000_000_000_u128,
            1_000_000_000_u128,
            CONCENTRATED_MIN_PEAK_DEPTH_NAD,
            100_u128,
            [1_u128, 1_000, 100_000],
        ),
        (
            1_000_000_000_000_000,
            1_000_000_000_000_000,
            PEAK_DEPTH_200,
            FADE_TENTH,
            [1, 1_000_000_000_000, 100_000_000_000_000],
        ),
        (
            1_000_000_000_000_000,
            1_200_000_000_000_000,
            PEAK_DEPTH_200,
            FADE_TENTH,
            [1, 50_000_000_000_000, 200_000_000_000_000],
        ),
        (
            u64::MAX as u128 - 1_000_000,
            u64::MAX as u128 - 2_000_000,
            CONCENTRATED_MAX_PEAK_DEPTH_NAD,
            CONCENTRATED_MAX_FADE_SCALE_NAD,
            [1, 131_071, 500_000],
        ),
    ] {
        let prepared = concentrated_prepare_curve(base, quote, NAD as u128, peak, fade).unwrap();
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            for amount_in in amounts {
                assert_inner_newton_quote_differential(prepared, amount_in, direction);
            }
        }
    }
}

#[test]
fn inner_newton_matches_prior_search_across_both_c1_joins() {
    for (peak_depth_nad, fade_scale_nad) in [
        (CONCENTRATED_MIN_PEAK_DEPTH_NAD, 100_u128),
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
            for (base, quote) in [(high_common, low_common), (low_common, high_common)] {
                let prepared =
                    concentrated_prepare_curve(base, quote, NAD as u128, peak_depth_nad, fade_scale_nad).unwrap();
                for direction in [
                    ConcentratedSwapDirection::BaseToQuote,
                    ConcentratedSwapDirection::QuoteToBase,
                ] {
                    for amount_in in [d / 1_000_000, d / 100_000, d / 10_000] {
                        assert_inner_newton_quote_differential(prepared, amount_in, direction);
                    }
                }
            }
        }
    }
}

#[test]
fn variable_reserve_newton_matches_canonical_bisection_across_quote_brackets() {
    let states = [
        (
            1_000_000_000_000_000_u128,
            1_000_000_000_000_000_u128,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        ),
        (
            1_000_000_000_000_000,
            1_350_000_000_000_000,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        ),
        (
            1_350_000_000_000_000,
            1_000_000_000_000_000,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        ),
        (
            8_000_000_000_000,
            1_000_000_000_000,
            2 * NAD as u128,
            PEAK_DEPTH_200,
            CONCENTRATED_MAX_FADE_SCALE_NAD,
        ),
        (
            1_000_000_000_000,
            8_000_000_000_000,
            NAD as u128 / 2,
            2 * NAD as u128,
            50_000_000,
        ),
        (
            u64::MAX as u128 / 4,
            u64::MAX as u128 / 5,
            NAD as u128,
            CONCENTRATED_MAX_PEAK_DEPTH_NAD,
            1,
        ),
    ];
    for (base, quote, center, peak, fade) in states {
        let prepared = concentrated_prepare_curve(base, quote, center, peak, fade).unwrap();
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            let (input_reserve, output_reserve, input_common, output_common) = match direction {
                ConcentratedSwapDirection::BaseToQuote => (base, quote, prepared.base_common, prepared.quote_common),
                ConcentratedSwapDirection::QuoteToBase => (quote, base, prepared.quote_common, prepared.base_common),
            };
            for divisor in [10_000_u128, 100, 4] {
                let amount_in = (input_reserve / divisor).max(1);
                let input_after = input_reserve.checked_add(amount_in).unwrap();
                let fixed = prepared
                    .input_common_scale(direction)
                    .unwrap()
                    .to_common_floor(input_after)
                    .unwrap();
                if fixed > input_common
                    && hybrid_residual(fixed, output_common, prepared.invariant_d, prepared.geometry)
                        .unwrap()
                        .0
                {
                    let accelerated =
                        solve_variable_reserve(fixed, prepared.invariant_d, prepared.geometry, 1, output_common)
                            .unwrap();
                    let reference = solve_variable_reserve_bisection_reference(
                        fixed,
                        prepared.invariant_d,
                        prepared.geometry,
                        1,
                        output_common,
                    )
                    .unwrap();
                    assert_eq!(accelerated, reference, "exact-in state/direction/divisor mismatch");
                }

                let amount_out = (output_reserve / divisor).max(1).min(output_reserve - 1);
                let output_after = output_reserve - amount_out;
                let fixed = prepared
                    .output_common_scale(direction)
                    .unwrap()
                    .to_common_floor(output_after)
                    .unwrap();
                if input_common < MAX_COMMON_RESERVE
                    && !hybrid_residual(fixed, input_common, prepared.invariant_d, prepared.geometry)
                        .unwrap()
                        .0
                    && hybrid_residual(fixed, MAX_COMMON_RESERVE, prepared.invariant_d, prepared.geometry)
                        .unwrap()
                        .0
                {
                    let accelerated = solve_variable_reserve(
                        fixed,
                        prepared.invariant_d,
                        prepared.geometry,
                        input_common,
                        MAX_COMMON_RESERVE,
                    )
                    .unwrap();
                    let reference = solve_variable_reserve_bisection_reference(
                        fixed,
                        prepared.invariant_d,
                        prepared.geometry,
                        input_common,
                        MAX_COMMON_RESERVE,
                    )
                    .unwrap();
                    assert_eq!(accelerated, reference, "exact-out state/direction/divisor mismatch");
                }
            }
        }
    }
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
    let branch = prepared_branch_at_raw_reserves(prepared, 130_000_000_000, 200_000_000_000 - output).unwrap();
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
    let expected = cpmm_amount_out_nad(x, y, dx).unwrap();
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
    while exact_cpmm_tail_in_raw(base, quote, high, direction, NAD as u128, PEAK_DEPTH_200, FADE_TENTH)
        .unwrap()
        .is_some()
    {
        low = high;
        high = high.checked_mul(2).unwrap();
    }
    while high - low > 1 {
        let probe = low + (high - low) / 2;
        if exact_cpmm_tail_in_raw(base, quote, probe, direction, NAD as u128, PEAK_DEPTH_200, FADE_TENTH)
            .unwrap()
            .is_some()
        {
            low = probe;
        } else {
            high = probe;
        }
    }

    let last_tail_output =
        concentrated_quote_exact_in(base, quote, low, direction, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let first_crossing_output =
        concentrated_quote_exact_in(base, quote, high, direction, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();

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
    let prepared = concentrated_prepare_curve(base, quote, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let output = prepared
        .quote_exact_in(input, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    let x_after = prepared.base_common.checked_add(input).unwrap();
    let y_after = prepared.quote_common.checked_sub(output).unwrap();

    assert!(
        hybrid_residual(x_after, y_after, prepared.invariant_d(), prepared.geometry,)
            .unwrap()
            .0
    );
    assert!(
        !hybrid_residual(x_after, y_after - 1, prepared.invariant_d(), prepared.geometry,)
            .unwrap()
            .0,
        "one additional output atom must be on the invalid side"
    );
}

#[test]
fn convergence_transition_matches_value_and_slope_at_both_joins() {
    let geometry = ConcentratedC1Geometry::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let (start_q, start_slope) = geometry.transition_q_and_slope_at_v(geometry.v_start_q48).unwrap();
    let (tail_q, tail_slope) = geometry.transition_q_and_slope_at_v(geometry.v_tail_q48).unwrap();
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
    assert!(
        concentrated_hybrid_branch_from_common(one_step_out.1, one_step_out.0, PEAK_DEPTH_200, FADE_TENTH,)
            .unwrap()
            .is_exact_tail()
    );
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
                let split_tail =
                    concentrated_prepare_curve(split_base, split_quote, center, PEAK_DEPTH_200, FADE_TENTH)
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
                        ConcentratedSwapDirection::BaseToQuote => {
                            (base + amount_in, quote - output, ConcentratedSwapDirection::QuoteToBase)
                        }
                        ConcentratedSwapDirection::QuoteToBase => {
                            (base - output, quote + amount_in, ConcentratedSwapDirection::BaseToQuote)
                        }
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
    let cpmm = cpmm_amount_out_nad(reserve, reserve, input).unwrap();
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
fn canonical_d_hint_adjacent_proof_is_exact_across_token_lattices_and_branches() {
    let geometry = ConcentratedC1Geometry::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let transition_v = geometry.v_start_q48 + (geometry.v_tail_q48 - geometry.v_start_q48) / 2;
    let (transition_q, _) = geometry.transition_q_and_slope_at_v(transition_v).unwrap();
    let transition = reserves_at_c1_coordinate(2_000_000_000_000_000, transition_q, transition_v);

    for decimals in [0_u8, 6, 9] {
        let atom_nad = normalize_to_nad(1, decimals).unwrap();
        let align = |value: u128| (value / atom_nad).max(1) * atom_nad;
        let fixtures = [
            (
                ConcentratedHybridBranch::Inner,
                align(1_000_000_000_000_000),
                align(1_050_000_000_000_000),
            ),
            (
                ConcentratedHybridBranch::BaseScarceTransition,
                align(transition.0),
                align(transition.1),
            ),
            (
                ConcentratedHybridBranch::QuoteScarceTransition,
                align(transition.1),
                align(transition.0),
            ),
            (
                ConcentratedHybridBranch::BaseScarceTail,
                align(1_000_000_000),
                align(8_000_000_000_000),
            ),
            (
                ConcentratedHybridBranch::QuoteScarceTail,
                align(8_000_000_000_000),
                align(1_000_000_000),
            ),
        ];

        for (expected_branch, base, quote) in fixtures {
            reset_residual_evaluations();
            let cold = concentrated_prepare_curve(base, quote, NAD as u128, PEAK_DEPTH_200, FADE_TENTH).unwrap();
            let cold_residuals = residual_evaluations();
            assert_eq!(
                geometry.branch(cold.base_common, cold.quote_common).unwrap(),
                expected_branch,
                "decimals={decimals} base={base} quote={quote}"
            );
            let canonical_d = cold.invariant_d();
            let cache = cold.geometry_cache().unwrap();

            reset_residual_evaluations();
            reset_canonical_d_hint_counters();
            let exact_hint = concentrated_prepare_curve_seeded_cached(
                base,
                quote,
                NAD as u128,
                PEAK_DEPTH_200,
                FADE_TENTH,
                cache,
                ConcentratedInvariantSeed::Hint(canonical_d),
            )
            .unwrap();
            assert_eq!(exact_hint, cold, "exact hint changed {expected_branch:?} at {decimals} decimals");
            assert_eq!(canonical_d_hint_counters(), (1, 0, 2));
            assert_eq!(residual_evaluations(), 2);
            assert!(
                cold_residuals > residual_evaluations(),
                "exact hint did not save residuals for {expected_branch:?} at {decimals} decimals"
            );

            let bracket_low = geometric_mean_floor(cold.base_common, cold.quote_common).unwrap() * 2;
            let bracket_high = cold.base_common + cold.quote_common;
            for hint in [
                canonical_d - 1,
                canonical_d + 1,
                bracket_low + (canonical_d - bracket_low) / 2,
                canonical_d + (bracket_high - canonical_d) / 2,
                0,
                u128::MAX,
            ] {
                if hint == canonical_d {
                    continue;
                }
                reset_canonical_d_hint_counters();
                let hinted = concentrated_prepare_curve_seeded_cached(
                    base,
                    quote,
                    NAD as u128,
                    PEAK_DEPTH_200,
                    FADE_TENTH,
                    cache,
                    ConcentratedInvariantSeed::Hint(hint),
                )
                .unwrap();
                assert_eq!(
                    hinted, cold,
                    "hint={hint} changed {expected_branch:?} at {decimals} decimals"
                );
                let (hits, misses, _) = canonical_d_hint_counters();
                assert_eq!((hits, misses), (0, 1));
            }
        }
    }
}

#[test]
fn canonical_d_hint_boundary_zero_and_error_domain_are_unchanged() {
    let geometry = ConcentratedC1Geometry::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap();
    let reserve = 1_000_000_000_000_u128;
    let context = ConcentratedResidualContext::derive(geometry, reserve, reserve).unwrap();
    reset_canonical_d_hint_counters();
    let boundary = canonical_d_hint_evidence(
        reserve,
        reserve,
        reserve * 2,
        reserve * 2,
        reserve * 2,
        Some(geometry),
        Some(context),
    )
    .unwrap();
    assert_eq!(boundary.hit, Some(reserve * 2));
    assert_eq!(canonical_d_hint_counters(), (1, 0, 1));

    let cache = ConcentratedGeometryCache::derive(PEAK_DEPTH_200, FADE_TENTH).unwrap();
    for (base, quote) in [
        (0, reserve),
        (MIN_INNER_COMMON_RESERVE - 1, MIN_INNER_COMMON_RESERVE - 1),
        (MAX_COMMON_RESERVE + 1, MAX_COMMON_RESERVE),
        (u128::MAX, reserve),
    ] {
        let cold_error = concentrated_prepare_curve(base, quote, NAD as u128, PEAK_DEPTH_200, FADE_TENTH)
            .unwrap_err();
        let cold_error_number = concentrated_error_number(cold_error);
        for hint in [0, reserve * 2, u128::MAX] {
            let hinted_error = concentrated_prepare_curve_seeded_cached(
                base,
                quote,
                NAD as u128,
                PEAK_DEPTH_200,
                FADE_TENTH,
                cache,
                ConcentratedInvariantSeed::Hint(hint),
            )
            .unwrap_err();
            assert_eq!(
                concentrated_error_number(hinted_error),
                cold_error_number,
                "base={base} quote={quote} hint={hint}"
            );
        }
    }
}

#[test]
fn canonical_d_hint_authority_like_successors_miss_safely_and_reuse_lower_bracket() {
    let base = 1_000_000_u128 * NAD as u128;
    let quote = 2_000_000_u128 * NAD as u128;
    let center = 2 * NAD as u128;
    for decimals in [0_u8, 6, 9] {
        let atom_nad = normalize_to_nad(1, decimals).unwrap();
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            let start = concentrated_prepare_curve(base, quote, center, PEAK_DEPTH_200, FADE_TENTH).unwrap();
            let amount_in = 350_000_u128 * NAD as u128;
            let solved_out = start.quote_exact_in(amount_in, direction).unwrap();
            let executable_out = solved_out - solved_out % atom_nad;
            let (trade_base, trade_quote) = match direction {
                ConcentratedSwapDirection::BaseToQuote => (base + amount_in, quote - executable_out),
                ConcentratedSwapDirection::QuoteToBase => (base - executable_out, quote + amount_in),
            };

            reset_residual_evaluations();
            let cold_trade = concentrated_prepare_curve(trade_base, trade_quote, center, PEAK_DEPTH_200, FADE_TENTH)
                .unwrap();
            let cold_trade_residuals = residual_evaluations();
            reset_residual_evaluations();
            reset_canonical_d_hint_counters();
            let warm_trade = start
                .prepare_successor(
                    trade_base,
                    trade_quote,
                    ConcentratedInvariantSeed::Hint(start.invariant_d()),
                )
                .unwrap();
            let warm_trade_residuals = residual_evaluations();
            let trade_counters = canonical_d_hint_counters();
            assert_eq!(warm_trade, cold_trade);
            assert!(cold_trade.invariant_d() > start.invariant_d());
            assert_eq!(trade_counters, (0, 1, 1));
            assert!(warm_trade_residuals < cold_trade_residuals);

            let retained = 3_500_u128 * NAD as u128;
            let (reserve_base, reserve_quote) = match direction {
                ConcentratedSwapDirection::BaseToQuote => (trade_base + retained, trade_quote),
                ConcentratedSwapDirection::QuoteToBase => (trade_base, trade_quote + retained),
            };
            reset_residual_evaluations();
            let cold_reserve =
                concentrated_prepare_curve(reserve_base, reserve_quote, center, PEAK_DEPTH_200, FADE_TENTH).unwrap();
            let cold_reserve_residuals = residual_evaluations();
            reset_residual_evaluations();
            reset_canonical_d_hint_counters();
            let warm_reserve = warm_trade
                .prepare_successor(
                    reserve_base,
                    reserve_quote,
                    ConcentratedInvariantSeed::Hint(warm_trade.invariant_d()),
                )
                .unwrap();
            let warm_reserve_residuals = residual_evaluations();
            let reserve_counters = canonical_d_hint_counters();
            assert_eq!(warm_reserve, cold_reserve);
            assert!(cold_reserve.invariant_d() > warm_trade.invariant_d());
            assert_eq!(reserve_counters, (0, 1, 1));
            assert!(warm_reserve_residuals <= cold_reserve_residuals);
        }
    }
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
        let cached =
            concentrated_prepare_curve_cached(1_000_000_000_000, 1_200_000_000_000, NAD as u128, peak, fade, cache)
                .unwrap();
        let cold = concentrated_prepare_curve(1_000_000_000_000, 1_200_000_000_000, NAD as u128, peak, fade).unwrap();
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

fn wide_balance_factor_reference(x: u128, y: u128, d: u128, fixed_one: u128) -> core::result::Result<u128, ()> {
    let maximum = U256::from(u128::MAX);
    let first_quotient = U256::from(x) * U256::from(2_u8) * U256::from(fixed_one) / U256::from(d);
    let quotient =
        U256::from(x) * U256::from(y) * U256::from(4_u8) * U256::from(fixed_one) / (U256::from(d) * U256::from(d));
    if first_quotient > maximum || quotient > maximum {
        Err(())
    } else {
        Ok(quotient.low_u128())
    }
}

#[test]
fn bounded_balance_factor_matches_staged_and_wide_boundaries() {
    let reserves = [
        1_u128,
        2,
        (1_u128 << 32) - 1,
        1_u128 << 32,
        (1_u128 << 63) - 1,
        1_u128 << 63,
        u64::MAX as u128,
    ];
    let invariants = [
        1_u128,
        2,
        (1_u128 << 32) - 1,
        1_u128 << 32,
        (1_u128 << 64) - 1,
        1_u128 << 64,
        (1_u128 << 64) + 1,
        2 * u64::MAX as u128,
    ];
    for x in reserves {
        for y in reserves {
            for d in invariants {
                for fixed_one in [Q64_ONE, Q80_ONE] {
                    let accelerated = balance_factor_fixed(x, y, d, fixed_one).map_err(|_| ());
                    let staged = balance_factor_fixed_staged(x, y, d, fixed_one).map_err(|_| ());
                    let reference = wide_balance_factor_reference(x, y, d, fixed_one);
                    assert_eq!(accelerated, staged, "staged x={x} y={y} d={d} fixed={fixed_one}");
                    assert_eq!(accelerated, reference, "wide x={x} y={y} d={d} fixed={fixed_one}");
                }
            }
        }
    }
}

#[test]
fn bounded_balance_factor_preserves_staged_overflow_boundaries_and_off_domain_fallback() {
    // The mathematical final Q80 value fits, but the legacy first staged
    // quotient does not. The specialization must retain that exact error.
    let first_stage_overflow = balance_factor_fixed(u64::MAX as u128, 1, 1_u128 << 16, Q80_ONE);
    assert!(first_stage_overflow.is_err());
    assert!(balance_factor_fixed_staged(u64::MAX as u128, 1, 1_u128 << 16, Q80_ONE).is_err());
    assert!(wide_balance_factor_reference(u64::MAX as u128, 1, 1_u128 << 16, Q80_ONE).is_err());

    // Here the first quotient fits and the final quotient does not.
    let final_overflow = balance_factor_fixed(u64::MAX as u128, u64::MAX as u128, 1_u128 << 32, Q64_ONE);
    assert!(final_overflow.is_err());
    assert!(balance_factor_fixed_staged(u64::MAX as u128, u64::MAX as u128, 1_u128 << 32, Q64_ONE,).is_err());

    // Non-authoritative scales and wider denominators retain the old path.
    for (d, fixed_one) in [
        (2 * u64::MAX as u128 + 1, Q64_ONE),
        (u128::MAX, Q80_ONE),
        (123_456_789, 1_u128 << 63),
    ] {
        let accelerated = balance_factor_fixed(u64::MAX as u128, u64::MAX as u128, d, fixed_one).map_err(|_| ());
        let staged = balance_factor_fixed_staged(u64::MAX as u128, u64::MAX as u128, d, fixed_one).map_err(|_| ());
        assert_eq!(accelerated, staged);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn bounded_balance_factor_is_exactly_differential(
        x_raw in 1_u64..=u64::MAX,
        y_raw in 1_u64..=u64::MAX,
        d_low in any::<u64>(),
        d_high in any::<bool>(),
        q80 in any::<bool>(),
    ) {
        let x = x_raw as u128;
        let y = y_raw as u128;
        let d = if d_high {
            (1_u128 << 64) + (d_low as u128 % u64::MAX as u128)
        } else {
            (d_low as u128).max(1)
        };
        let fixed_one = if q80 { Q80_ONE } else { Q64_ONE };
        let accelerated = balance_factor_fixed(x, y, d, fixed_one).map_err(|_| ());
        let staged = balance_factor_fixed_staged(x, y, d, fixed_one).map_err(|_| ());
        let reference = wide_balance_factor_reference(x, y, d, fixed_one);

        prop_assert_eq!(accelerated, staged);
        prop_assert_eq!(accelerated, reference);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn inner_newton_is_differentially_identical_on_random_inner_quotes(
        reserve in 1_000_000_000_000_u128..100_000_000_000_000_000_u128,
        quote_ratio_bps in 9_900_u128..=10_100_u128,
        input_bps in 1_u128..=100_u128,
        base_to_quote in any::<bool>(),
    ) {
        let base = reserve;
        let quote = mul_div_floor(reserve, quote_ratio_bps, 10_000).unwrap();
        let amount_in = mul_div_floor(reserve, input_bps, 10_000).unwrap().max(1);
        let direction = if base_to_quote {
            ConcentratedSwapDirection::BaseToQuote
        } else {
            ConcentratedSwapDirection::QuoteToBase
        };
        let prepared = concentrated_prepare_curve(
            base,
            quote,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_TENTH,
        )
        .unwrap();
        assert_inner_newton_quote_differential(prepared, amount_in, direction);
    }

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
