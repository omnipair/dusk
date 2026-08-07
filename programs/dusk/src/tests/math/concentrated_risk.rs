use super::*;
use crate::math::{
    concentrated_hybrid_branch_from_common, concentrated_prepare_curve, concentrated_quote_exact_in,
    ConcentratedHybridBranch,
};

const PEAK_DEPTH_200: u128 = 200 * NAD as u128;
const FADE_SCALE_TENTH: u128 = NAD as u128 / 10;

fn shape_marginal_nad(balance_factor_nad: u128, peak_depth_nad: u128, fade_scale_nad: u128) -> Result<u128> {
    SHAPE_EVALUATIONS.with(|count| count.set(count.get() + 1));
    let (t, z) = shape_coordinates(balance_factor_nad, peak_depth_nad, fade_scale_nad)?;
    let x = t.checked_add(z).ok_or(ErrorCode::InvariantOverflow)?;
    let y = t.checked_sub(z).ok_or(ErrorCode::InvariantOverflow)?;
    require!(y > 0, ErrorCode::InsufficientLiquidity);
    concentrated_marginal_price_from_common(
        x,
        y,
        SHAPE_SCALE.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
        NAD as u128,
        peak_depth_nad,
        fade_scale_nad,
    )
}

fn recovered(
    target: u128,
    q: u128,
    direction: ConcentratedSwapDirection,
    center: u128,
    peak_depth: u128,
    fade_scale: u128,
) -> (ConcentratedRiskReserves, u128, u128) {
    let reserves = concentrated_risk_reserves_at_price_q(target, q, direction, center, peak_depth, fade_scale).unwrap();
    let amount_in = match direction {
        ConcentratedSwapDirection::BaseToQuote => reserves.base_reserve_nad,
        ConcentratedSwapDirection::QuoteToBase => reserves.quote_reserve_nad,
    }
    .checked_div(1_000_000)
    .unwrap()
    .max(1);
    let amount_out = concentrated_quote_exact_in(
        reserves.base_reserve_nad,
        reserves.quote_reserve_nad,
        amount_in,
        direction,
        center,
        peak_depth,
        fade_scale,
    )
    .unwrap();
    let executable_price = match direction {
        ConcentratedSwapDirection::BaseToQuote => {
            if amount_out == 0 {
                0
            } else {
                mul_div_floor(amount_out, NAD as u128, amount_in).unwrap()
            }
        }
        ConcentratedSwapDirection::QuoteToBase => {
            if amount_out == 0 {
                u128::MAX
            } else {
                mul_div_ceil(amount_in, NAD as u128, amount_out).unwrap()
            }
        }
    };
    let recovered_q = concentrated_prepare_curve(
        reserves.base_reserve_nad,
        reserves.quote_reserve_nad,
        center,
        peak_depth,
        fade_scale,
    )
    .unwrap()
    .balanced_equivalent_q()
    .unwrap();
    (reserves, executable_price, recovered_q)
}

#[test]
fn zero_peak_depth_matches_closed_form_cpmm_snapshot() {
    let target = NAD as u128 / 2;
    let q = 1_000_000 * NAD as u128;
    let reserves =
        concentrated_risk_reserves_at_price_q(target, q, ConcentratedSwapDirection::BaseToQuote, 7 * NAD as u128, 0, 0)
            .unwrap();
    let safe_q = conservative_q(q).unwrap();
    let expected_base =
        sqrt_ceil_u256_to_u128(U256::from(safe_q) * U256::from(safe_q) * U256::from(NAD) / U256::from(target)).unwrap();
    let expected_quote =
        sqrt_floor_u256_to_u128(U256::from(safe_q) * U256::from(safe_q) * U256::from(target) / U256::from(NAD))
            .unwrap();

    assert_eq!(reserves.base_reserve_nad, expected_base);
    assert_eq!(reserves.quote_reserve_nad, expected_quote);
    let price = reserves.quote_reserve_nad * NAD as u128 / reserves.base_reserve_nad;
    assert!(price <= target);
}

#[test]
fn configured_input_bounds_require_u256_but_fit_it() {
    let q = MAX_BALANCED_EQUIVALENT_Q_NAD;
    let largest_exact_numerator = U256::from(q) * U256::from(q) * U256::from(u64::MAX) * U256::from(4_u8);
    assert!(largest_exact_numerator.bits() <= 254);
    assert!(largest_exact_numerator > U256::from(u128::MAX));

    for target in [1_u128, u64::MAX as u128] {
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            let reserves = concentrated_risk_reserves_at_price_q(target, q, direction, NAD as u128, 0, 0).unwrap();
            assert!(reserves.base_reserve_nad > 0);
            assert!(reserves.quote_reserve_nad > 0);
        }
    }

    assert!(
        concentrated_risk_reserves_at_price_q(1, q + 1, ConcentratedSwapDirection::BaseToQuote, NAD as u128, 0, 0,)
            .is_err()
    );
    assert!(concentrated_risk_reserves_at_price_q(
        u64::MAX as u128 + 1,
        q,
        ConcentratedSwapDirection::BaseToQuote,
        NAD as u128,
        0,
        0,
    )
    .is_err());
}

#[test]
fn balanced_shape_is_exact_at_center() {
    assert_eq!(
        shape_coordinates(NAD as u128, PEAK_DEPTH_200, FADE_SCALE_TENTH).unwrap(),
        (SHAPE_SCALE, 0)
    );
    assert_eq!(
        shape_marginal_nad(NAD as u128, PEAK_DEPTH_200, FADE_SCALE_TENTH).unwrap(),
        NAD as u128
    );
}

#[test]
fn concentrated_snapshot_recovers_price_and_never_overstates_q() {
    let q = 1_000_000 * NAD as u128;
    let center = 2 * NAD as u128;
    for direction in [
        ConcentratedSwapDirection::BaseToQuote,
        ConcentratedSwapDirection::QuoteToBase,
    ] {
        for target in [center / 20, center / 2, center, center * 2, center * 20] {
            let (_, price, recovered_q) = recovered(target, q, direction, center, PEAK_DEPTH_200, FADE_SCALE_TENTH);
            let error_ppm = price.abs_diff(target) * PPM_DENOMINATOR / target;
            assert!(error_ppm <= 1_000, "target={target} price={price} error={error_ppm}ppm");
            assert!(recovered_q <= q, "target={target} recovered_q={recovered_q} q={q}");
            assert!(recovered_q >= q * (PPM_DENOMINATOR - 100) / PPM_DENOMINATOR);
            match direction {
                ConcentratedSwapDirection::BaseToQuote => assert!(price <= target),
                ConcentratedSwapDirection::QuoteToBase => assert!(price >= target),
            }
        }
    }
}

#[test]
fn reconstruction_is_homogeneous() {
    let center = NAD as u128;
    let q = 500_000 * NAD as u128;
    // Deep tail, shoulder kink, and inner shoulder cover all hybrid regions.
    for target in [center / 10, center * 3 / 5, center * 99 / 100] {
        let small = concentrated_risk_reserves_at_price_q(
            target,
            q,
            ConcentratedSwapDirection::BaseToQuote,
            center,
            PEAK_DEPTH_200,
            FADE_SCALE_TENTH,
        )
        .unwrap();
        let large = concentrated_risk_reserves_at_price_q(
            target,
            q * 7,
            ConcentratedSwapDirection::BaseToQuote,
            center,
            PEAK_DEPTH_200,
            FADE_SCALE_TENTH,
        )
        .unwrap();

        // The production shoulder is Q48. Independent directed rounding at
        // the two depths can compound by a few normalized atoms when the
        // smaller snapshot is multiplied back by seven.
        assert!(
            large.base_reserve_nad.abs_diff(small.base_reserve_nad * 7) <= 16,
            "target={target} small={} large={} delta={}",
            small.base_reserve_nad,
            large.base_reserve_nad,
            large.base_reserve_nad.abs_diff(small.base_reserve_nad * 7),
        );
        assert!(
            large.quote_reserve_nad.abs_diff(small.quote_reserve_nad * 7) <= 16,
            "target={target} small={} large={} delta={}",
            small.quote_reserve_nad,
            large.quote_reserve_nad,
            large.quote_reserve_nad.abs_diff(small.quote_reserve_nad * 7),
        );
    }
}

#[test]
fn deep_tail_reconstruction_uses_exact_cpmm_execution() {
    let q = 1_000_000 * NAD as u128;
    let center = NAD as u128;
    for (target, direction, expected_branch) in [
        (
            center / 25,
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedHybridBranch::QuoteScarceTail,
        ),
        (
            center * 25,
            ConcentratedSwapDirection::QuoteToBase,
            ConcentratedHybridBranch::BaseScarceTail,
        ),
    ] {
        let reserves =
            concentrated_risk_reserves_at_price_q(target, q, direction, center, PEAK_DEPTH_200, FADE_SCALE_TENTH)
                .unwrap();
        assert_eq!(
            concentrated_hybrid_branch_from_common(
                reserves.base_reserve_nad,
                reserves.quote_reserve_nad,
                PEAK_DEPTH_200,
                FADE_SCALE_TENTH,
            )
            .unwrap(),
            expected_branch
        );

        let (input_reserve, output_reserve) = match direction {
            ConcentratedSwapDirection::BaseToQuote => (reserves.base_reserve_nad, reserves.quote_reserve_nad),
            ConcentratedSwapDirection::QuoteToBase => (reserves.quote_reserve_nad, reserves.base_reserve_nad),
        };
        let amount_in = (input_reserve / 100).max(1);
        let hybrid_output = concentrated_quote_exact_in(
            reserves.base_reserve_nad,
            reserves.quote_reserve_nad,
            amount_in,
            direction,
            center,
            PEAK_DEPTH_200,
            FADE_SCALE_TENTH,
        )
        .unwrap();
        let cpmm_output =
            crate::math::cpmm_amount_out_nad(input_reserve, output_reserve, amount_in).unwrap();
        assert_eq!(hybrid_output, cpmm_output, "target={target} direction={direction:?}");
    }
}

#[test]
fn convergence_transition_prices_are_reconstructible() {
    let q = 1_000_000 * NAD as u128;
    let center = NAD as u128;
    let geometry = ConcentratedC1Geometry::derive(PEAK_DEPTH_200, FADE_SCALE_TENTH).unwrap();
    let shape_d = SHAPE_SCALE * 2;
    let marginal = |v_q48, q_q48| {
        let (low_factor, high_factor) = transition_shape_factors_q48(q_q48, v_q48).unwrap();
        let low = mul_div_floor(SHAPE_SCALE, low_factor, Q48_ONE).unwrap();
        let high = mul_div_floor(SHAPE_SCALE, high_factor, Q48_ONE).unwrap();
        concentrated_marginal_price_from_common(
            high,
            low,
            shape_d,
            center,
            PEAK_DEPTH_200,
            FADE_SCALE_TENTH,
        )
        .unwrap()
    };
    let start = marginal(geometry.v_start_q48, geometry.q_start_q48);
    let tail = marginal(geometry.v_tail_q48, geometry.q_tail_q48);
    let target = tail + (start - tail) / 2;
    for direction in [
        ConcentratedSwapDirection::BaseToQuote,
        ConcentratedSwapDirection::QuoteToBase,
    ] {
        let (reserves, executable_price, recovered_q) =
            recovered(target, q, direction, center, PEAK_DEPTH_200, FADE_SCALE_TENTH);
        assert!(matches!(
            concentrated_hybrid_branch_from_common(
                reserves.base_reserve_nad,
                reserves.quote_reserve_nad,
                PEAK_DEPTH_200,
                FADE_SCALE_TENTH,
            )
            .unwrap(),
            ConcentratedHybridBranch::BaseScarceTransition
                | ConcentratedHybridBranch::QuoteScarceTransition
        ));
        assert!(recovered_q <= q);
        match direction {
            ConcentratedSwapDirection::BaseToQuote => assert!(executable_price <= target),
            ConcentratedSwapDirection::QuoteToBase => assert!(executable_price >= target),
        }
    }
}

#[test]
fn hybrid_reconstruction_is_symmetric_across_center() {
    let q = 1_000_000 * NAD as u128;
    let center = NAD as u128;
    for (low_direction, high_direction) in [
        (
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ),
        (
            ConcentratedSwapDirection::QuoteToBase,
            ConcentratedSwapDirection::BaseToQuote,
        ),
    ] {
        let low = concentrated_risk_reserves_at_price_q(
            center / 20,
            q,
            low_direction,
            center,
            PEAK_DEPTH_200,
            FADE_SCALE_TENTH,
        )
        .unwrap();
        let high = concentrated_risk_reserves_at_price_q(
            center * 20,
            q,
            high_direction,
            center,
            PEAK_DEPTH_200,
            FADE_SCALE_TENTH,
        )
        .unwrap();
        assert_eq!(low.base_reserve_nad, high.quote_reserve_nad);
        assert_eq!(low.quote_reserve_nad, high.base_reserve_nad);
    }
}

#[test]
fn both_directions_and_parameter_extremes_are_bounded() {
    let q = 2_000_000 * NAD as u128;
    let center = NAD as u128;
    for (peak_depth, fade_scale) in [
        (2 * NAD as u128, 10),
        (PEAK_DEPTH_200, FADE_SCALE_TENTH),
        (2_000 * NAD as u128, CONCENTRATED_MAX_FADE_SCALE_NAD),
    ] {
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            for target in [
                center / 100,
                center / 20,
                center / 2,
                center * 2,
                center * 20,
                center * 100,
            ] {
                reset_risk_shape_counters();
                let (_reserves, price, recovered_q) = recovered(target, q, direction, center, peak_depth, fade_scale);
                let (shape_evaluations, sqrt_iterations) = risk_shape_counters();
                if price != 0 && price != u128::MAX {
                    let error_ppm = price.abs_diff(target) * PPM_DENOMINATOR / target;
                    assert!(
                        error_ppm <= 3_000,
                        "peak_depth={peak_depth} fade_scale={fade_scale} target={target} price={price} error={error_ppm}ppm"
                    );
                }
                assert!(
                    recovered_q <= q,
                    "center={center} target={target} direction={direction:?} recovered_q={recovered_q} q={q}"
                );
                match direction {
                    ConcentratedSwapDirection::BaseToQuote => {
                        assert!(
                            price <= target,
                            "peak_depth={peak_depth} fade_scale={fade_scale} target={target} price={price}"
                        );
                    }
                    ConcentratedSwapDirection::QuoteToBase => {
                        assert!(
                            price >= target,
                            "peak_depth={peak_depth} fade_scale={fade_scale} target={target} price={price}"
                        );
                    }
                }
                assert!(
                    shape_evaluations <= CONCENTRATED_RISK_PRICE_MAX_ITERS,
                    "shape evaluations={shape_evaluations}"
                );
                assert!(
                    sqrt_iterations <= (CONCENTRATED_RISK_PRICE_MAX_ITERS + 7) * CONCENTRATED_RISK_SQRT_MAX_ITERS,
                    "sqrt iterations={sqrt_iterations}"
                );
            }
        }
    }
}

#[test]
fn integer_u256_newton_sqrt_is_exact_and_structurally_bounded() {
    let mut worst_iterations = 0;
    let mut roots = Vec::with_capacity(3 * 128);
    for bits in 1..=128 {
        let minimum = 1_u128 << (bits - 1);
        let maximum = if bits == 128 { u128::MAX } else { (1_u128 << bits) - 1 };
        roots.extend([minimum, minimum + (maximum - minimum) / 2, maximum]);
    }

    for root in roots {
        let square = U256::from(root) * U256::from(root);
        for radicand in [
            square.saturating_sub(U256::one()),
            square,
            square.saturating_add(U256::one()),
        ] {
            if radicand.is_zero() || radicand > U256::from(u128::MAX) * U256::from(u128::MAX) {
                continue;
            }
            reset_risk_shape_counters();
            let floor = sqrt_floor_u256_to_u128(radicand).unwrap();
            let (_, iterations) = risk_shape_counters();
            worst_iterations = worst_iterations.max(iterations);

            assert!(U256::from(floor) * U256::from(floor) <= radicand);
            if floor < u128::MAX {
                let successor = floor + 1;
                assert!(U256::from(successor) * U256::from(successor) > radicand);
            }
        }
    }

    assert!(worst_iterations <= 10, "sqrt iterations={worst_iterations}");
    let overflow = U256::from(u128::MAX) * U256::from(u128::MAX) + U256::one();
    assert!(sqrt_floor_u256_to_u128(overflow).is_err());
}

#[test]
fn direction_is_conservative_in_all_price_quadrants_and_at_center() {
    let q = 1_000_000 * NAD as u128;
    let center = 3 * NAD as u128;
    for direction in [
        ConcentratedSwapDirection::BaseToQuote,
        ConcentratedSwapDirection::QuoteToBase,
    ] {
        for target in [center / 2, center, center * 2] {
            let (reserves, price, _) = recovered(target, q, direction, center, PEAK_DEPTH_200, FADE_SCALE_TENTH);
            match direction {
                ConcentratedSwapDirection::BaseToQuote => assert!(price <= target),
                ConcentratedSwapDirection::QuoteToBase => assert!(price >= target),
            }

            if target == center {
                let amount_in = q / 1_000;
                let amount_out = concentrated_quote_exact_in(
                    reserves.base_reserve_nad,
                    reserves.quote_reserve_nad,
                    amount_in,
                    direction,
                    center,
                    PEAK_DEPTH_200,
                    FADE_SCALE_TENTH,
                )
                .unwrap();
                let linear_value = match direction {
                    ConcentratedSwapDirection::BaseToQuote => amount_in * target / NAD as u128,
                    ConcentratedSwapDirection::QuoteToBase => amount_in * NAD as u128 / target,
                };
                assert!(amount_out <= linear_value);
            }
        }
    }
}

#[test]
fn adaptive_numeraire_risk_reconstruction_replays_conservatively() {
    let q = 1_000_000_000_000_000_u128;
    for center in [1_000_000_u128, 1_000_000_000_000_000_u128] {
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            for target in [center / 2, center, center * 2] {
                let (reserves, executable_price, recovered_q) =
                    recovered(target, q, direction, center, PEAK_DEPTH_200, FADE_SCALE_TENTH);
                assert!(reserves.base_reserve_nad > 0 && reserves.quote_reserve_nad > 0);
                assert!(
                    recovered_q <= q,
                    "center={center} target={target} direction={direction:?} recovered_q={recovered_q} q={q}"
                );
                assert!(recovered_q >= q * (PPM_DENOMINATOR - 100) / PPM_DENOMINATOR);
                match direction {
                    ConcentratedSwapDirection::BaseToQuote => assert!(
                        executable_price <= target,
                        "center={center} target={target} replay={executable_price}"
                    ),
                    ConcentratedSwapDirection::QuoteToBase => assert!(
                        executable_price >= target,
                        "center={center} target={target} replay={executable_price}"
                    ),
                }
            }
        }
    }
}

#[test]
fn central_shape_marginal_tracks_executable_quotes_inside_shoulder() {
    for balance_factor in [
        NAD as u128 * 999 / 1_000,
        NAD as u128 * 99 / 100,
        NAD as u128 * 95 / 100,
        NAD as u128 * 91 / 100,
    ] {
        let (t_scaled, z_scaled) = shape_coordinates(balance_factor, PEAK_DEPTH_200, FADE_SCALE_TENTH).unwrap();
        let d = 1_000_000 * NAD as u128;
        let x = mul_div_floor(d, t_scaled + z_scaled, 2 * SHAPE_SCALE).unwrap();
        let y = mul_div_floor(d, t_scaled - z_scaled, 2 * SHAPE_SCALE).unwrap();
        let shape_price = shape_marginal_nad(balance_factor, PEAK_DEPTH_200, FADE_SCALE_TENTH).unwrap();
        let base_in = (x / 1_000_000).max(1);
        let quote_in = (y / 1_000_000).max(1);
        let bid_out = concentrated_quote_exact_in(
            x,
            y,
            base_in,
            ConcentratedSwapDirection::BaseToQuote,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_SCALE_TENTH,
        )
        .unwrap();
        let ask_out = concentrated_quote_exact_in(
            x,
            y,
            quote_in,
            ConcentratedSwapDirection::QuoteToBase,
            NAD as u128,
            PEAK_DEPTH_200,
            FADE_SCALE_TENTH,
        )
        .unwrap();
        let bid = mul_div_floor(bid_out, NAD as u128, base_in).unwrap();
        let ask = if ask_out == 0 {
            u128::MAX
        } else {
            mul_div_ceil(quote_in, NAD as u128, ask_out).unwrap()
        };
        if bid > 0 {
            let bid_error_ppm = bid.abs_diff(shape_price) * PPM_DENOMINATOR / shape_price;
            assert!(
                bid_error_ppm <= 500,
                "balance={balance_factor} bid={bid} shape={shape_price} error={bid_error_ppm}ppm"
            );
        }
        if ask != u128::MAX {
            let ask_error_ppm = ask.abs_diff(shape_price) * PPM_DENOMINATOR / shape_price;
            assert!(
                ask_error_ppm <= 500,
                "balance={balance_factor} shape={shape_price} ask={ask} error={ask_error_ppm}ppm"
            );
        }
    }
}
