use super::*;
use crate::math::{
    concentrated_balanced_equivalent_q, concentrated_hybrid_branch_from_common, concentrated_invariant,
    concentrated_quote_exact_in, ConcentratedHybridBranch,
};

const PEAK_DEPTH_200: u128 = 200 * NAD as u128;
const IMBALANCE_SCALE_TENTH: u128 = NAD as u128 / 10;

fn recovered(
    target: u128,
    q: u128,
    direction: ConcentratedSwapDirection,
    center: u128,
    peak_depth: u128,
    imbalance_scale: u128,
) -> (ConcentratedRiskReserves, u128, u128) {
    let reserves =
        concentrated_risk_reserves_at_price_q(target, q, direction, center, peak_depth, imbalance_scale).unwrap();
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
        imbalance_scale,
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
    let invariant_d = concentrated_invariant(
        reserves.base_reserve_nad,
        reserves.quote_reserve_nad,
        center,
        peak_depth,
        imbalance_scale,
    )
    .unwrap();
    let recovered_q = concentrated_balanced_equivalent_q(invariant_d, center).unwrap();
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
        sqrt_ceil_u512_to_u128(U512::from(safe_q) * U512::from(safe_q) * U512::from(NAD) / U512::from(target)).unwrap();
    let expected_quote =
        sqrt_floor_u512_to_u128(U512::from(safe_q) * U512::from(safe_q) * U512::from(target) / U512::from(NAD))
            .unwrap();

    assert_eq!(reserves.base_reserve_nad, expected_base);
    assert_eq!(reserves.quote_reserve_nad, expected_quote);
    let price = reserves.quote_reserve_nad * NAD as u128 / reserves.base_reserve_nad;
    assert!(price <= target);
}

#[test]
fn balanced_shape_is_exact_at_center() {
    assert_eq!(
        shape_coordinates(NAD as u128, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap(),
        (SHAPE_SCALE, 0)
    );
    assert_eq!(
        shape_marginal_nad(NAD as u128, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap(),
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
            let (_, price, recovered_q) =
                recovered(target, q, direction, center, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH);
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
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let large = concentrated_risk_reserves_at_price_q(
            target,
            q * 7,
            ConcentratedSwapDirection::BaseToQuote,
            center,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();

        assert!(
            large.base_reserve_nad.abs_diff(small.base_reserve_nad * 7) <= 8,
            "target={target}"
        );
        assert!(
            large.quote_reserve_nad.abs_diff(small.quote_reserve_nad * 7) <= 8,
            "target={target}"
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
            concentrated_risk_reserves_at_price_q(target, q, direction, center, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH)
                .unwrap();
        assert_eq!(
            concentrated_hybrid_branch_from_common(
                reserves.base_reserve_nad,
                reserves.quote_reserve_nad,
                PEAK_DEPTH_200,
                IMBALANCE_SCALE_TENTH,
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
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let cpmm_output =
            crate::math::calculate_normalized_amount_out(input_reserve, output_reserve, amount_in).unwrap();
        assert_eq!(hybrid_output, cpmm_output, "target={target} direction={direction:?}");
    }
}

#[test]
fn marginal_kink_maps_to_the_single_shared_shoulder() {
    let q = 1_000_000 * NAD as u128;
    let center = NAD as u128;
    let safe_q = conservative_q(q).unwrap();
    let d = invariant_d_for_q(safe_q, center).unwrap();
    let shoulder = concentrated_hybrid_shoulder_from_d(d, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
    assert!(shoulder.tail_low_marginal_nad < shoulder.inner_low_marginal_nad);
    let gap_low_marginal =
        shoulder.tail_low_marginal_nad + (shoulder.inner_low_marginal_nad - shoulder.tail_low_marginal_nad) / 2;
    let high_gap_target = mul_div_ceil(center, NAD as u128, gap_low_marginal).unwrap();

    for (target, expected_base, expected_quote) in [
        (gap_low_marginal, shoulder.high_common, shoulder.low_common),
        (high_gap_target, shoulder.low_common, shoulder.high_common),
    ] {
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            let (reserves, executable_price, recovered_q) =
                recovered(target, q, direction, center, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH);
            assert_eq!(reserves.base_reserve_nad, expected_base);
            assert_eq!(reserves.quote_reserve_nad, expected_quote);
            assert!(recovered_q <= q);

            // Base collateral is valued by base->quote execution and may
            // never receive more than the requested price. Quote collateral
            // is valued by quote->base execution and may never acquire base
            // more cheaply than the requested canonical price.
            match direction {
                ConcentratedSwapDirection::BaseToQuote => assert!(
                    executable_price <= target,
                    "target={target} executable={executable_price}"
                ),
                ConcentratedSwapDirection::QuoteToBase => assert!(
                    executable_price >= target,
                    "target={target} executable={executable_price}"
                ),
            }
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
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let high = concentrated_risk_reserves_at_price_q(
            center * 20,
            q,
            high_direction,
            center,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
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
    for (peak_depth, imbalance_scale) in [
        (2 * NAD as u128, 10),
        (PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH),
        (2_000 * NAD as u128, CONCENTRATED_MAX_IMBALANCE_SCALE_NAD),
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
                let (reserves, price, recovered_q) =
                    recovered(target, q, direction, center, peak_depth, imbalance_scale);
                let (shape_evaluations, sqrt_iterations) = risk_shape_counters();
                let below_center =
                    target < center || (target == center && direction == ConcentratedSwapDirection::BaseToQuote);
                let low_marginal_nad = if below_center {
                    target
                } else {
                    mul_div_floor(center, NAD as u128, target).unwrap()
                };
                let move_away_from_center = matches!(
                    (below_center, direction),
                    (true, ConcentratedSwapDirection::BaseToQuote) | (false, ConcentratedSwapDirection::QuoteToBase)
                );
                let directed_marginal_nad = if move_away_from_center {
                    mul_div_floor(
                        low_marginal_nad,
                        PPM_DENOMINATOR - CONCENTRATED_RISK_PRICE_SAFETY_PPM,
                        PPM_DENOMINATOR,
                    )
                    .unwrap()
                    .max(1)
                } else {
                    mul_div_ceil(
                        low_marginal_nad,
                        PPM_DENOMINATOR + CONCENTRATED_RISK_PRICE_SAFETY_PPM,
                        PPM_DENOMINATOR,
                    )
                    .unwrap()
                    .min(NAD as u128)
                };
                let safe_q = conservative_q(q).unwrap();
                let d = invariant_d_for_q(safe_q, center).unwrap();
                let shoulder = concentrated_hybrid_shoulder_from_d(d, peak_depth, imbalance_scale).unwrap();
                let maps_to_marginal_gap = directed_marginal_nad >= shoulder.tail_low_marginal_nad
                    && directed_marginal_nad < shoulder.inner_low_marginal_nad;
                if maps_to_marginal_gap {
                    let (expected_base, expected_quote) = if below_center {
                        (shoulder.high_common, shoulder.low_common)
                    } else {
                        (shoulder.low_common, shoulder.high_common)
                    };
                    assert_eq!(reserves.base_reserve_nad, expected_base);
                    assert_eq!(reserves.quote_reserve_nad, expected_quote);
                } else if price != 0 && price != u128::MAX {
                    let error_ppm = price.abs_diff(target) * PPM_DENOMINATOR / target;
                    assert!(
                        error_ppm <= 3_000,
                        "peak_depth={peak_depth} imbalance_scale={imbalance_scale} target={target} price={price} error={error_ppm}ppm"
                    );
                }
                assert!(recovered_q <= q);
                match direction {
                    ConcentratedSwapDirection::BaseToQuote => {
                        assert!(
                            price <= target,
                            "peak_depth={peak_depth} imbalance_scale={imbalance_scale} target={target} price={price}"
                        );
                    }
                    ConcentratedSwapDirection::QuoteToBase => {
                        assert!(
                            price >= target,
                            "peak_depth={peak_depth} imbalance_scale={imbalance_scale} target={target} price={price}"
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
fn integer_newton_sqrt_is_exact_and_structurally_bounded() {
    let mut worst_iterations = 0;
    let mut roots = Vec::with_capacity(3 * 128);
    for bits in 1..=128 {
        let minimum = 1_u128 << (bits - 1);
        let maximum = if bits == 128 { u128::MAX } else { (1_u128 << bits) - 1 };
        roots.extend([minimum, minimum + (maximum - minimum) / 2, maximum]);
    }

    for root in roots {
        let square = U512::from(root) * U512::from(root);
        for radicand in [
            square.saturating_sub(U512::one()),
            square,
            square.saturating_add(U512::one()),
        ] {
            if radicand.is_zero() || radicand > U512::from(u128::MAX) * U512::from(u128::MAX) {
                continue;
            }
            reset_risk_shape_counters();
            let floor = sqrt_floor_u512_to_u128(radicand).unwrap();
            let (_, iterations) = risk_shape_counters();
            worst_iterations = worst_iterations.max(iterations);

            assert!(U512::from(floor) * U512::from(floor) <= radicand);
            if floor < u128::MAX {
                let successor = floor + 1;
                assert!(U512::from(successor) * U512::from(successor) > radicand);
            }
        }
    }

    assert!(worst_iterations <= 10, "sqrt iterations={worst_iterations}");
    let overflow = U512::from(u128::MAX) * U512::from(u128::MAX) + U512::one();
    assert!(sqrt_floor_u512_to_u128(overflow).is_err());
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
            let (reserves, price, _) = recovered(target, q, direction, center, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH);
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
                    IMBALANCE_SCALE_TENTH,
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
fn central_shape_marginal_tracks_executable_quotes_inside_shoulder() {
    for balance_factor in [
        NAD as u128 * 999 / 1_000,
        NAD as u128 * 99 / 100,
        NAD as u128 * 95 / 100,
        NAD as u128 * 91 / 100,
    ] {
        let (t_scaled, z_scaled) = shape_coordinates(balance_factor, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let d = 1_000_000 * NAD as u128;
        let x = mul_div_floor(d, t_scaled + z_scaled, 2 * SHAPE_SCALE).unwrap();
        let y = mul_div_floor(d, t_scaled - z_scaled, 2 * SHAPE_SCALE).unwrap();
        let shape_price = shape_marginal_nad(balance_factor, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let base_in = (x / 1_000_000).max(1);
        let quote_in = (y / 1_000_000).max(1);
        let bid_out = concentrated_quote_exact_in(
            x,
            y,
            base_in,
            ConcentratedSwapDirection::BaseToQuote,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let ask_out = concentrated_quote_exact_in(
            x,
            y,
            quote_in,
            ConcentratedSwapDirection::QuoteToBase,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
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
