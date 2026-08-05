use super::*;
use crate::math::concentrated_quote_exact_out_input_lower_bound;

fn assert_risk_composition_is_conservative(
    curve: ConcentratedRiskCurve,
    direction: ConcentratedSwapDirection,
    existing_debt: u128,
    collateral: u128,
) {
    let exact_out_upper = curve.exact_out(existing_debt, direction).unwrap();
    let utilized_lower = concentrated_quote_exact_out_input_lower_bound(
        curve.base_reserve_nad,
        curve.quote_reserve_nad,
        existing_debt,
        direction,
        curve.center_price_nad,
        curve.peak_depth_nad,
        curve.fade_scale_nad,
    )
    .unwrap();
    assert!(utilized_lower <= exact_out_upper);
    assert!(curve.exact_in(exact_out_upper, direction).unwrap() >= existing_debt);

    let mut low = 0_u128;
    let mut high = exact_out_upper;
    while low < high {
        let midpoint = low + (high - low) / 2;
        if curve.exact_in(midpoint, direction).unwrap() >= existing_debt {
            high = midpoint;
        } else {
            low = midpoint + 1;
        }
    }
    let reference_utilized = low;
    assert!(utilized_lower <= reference_utilized);

    let terms = pessimistic_max_debt_on_curve_nad(collateral, curve, direction, existing_debt).unwrap();
    let reference_max_total = curve.exact_in(reference_utilized + collateral, direction).unwrap();
    let reference_user_max = reference_max_total.saturating_sub(existing_debt);
    let value = curve.exact_in(collateral, direction).unwrap();
    let reference_liquidation_cf = reference_user_max
        .saturating_mul(BPS_DENOMINATOR as u128)
        .checked_div(value)
        .unwrap_or(0)
        .min(MAX_COLLATERAL_FACTOR_BPS as u128) as u16;
    let reference_max_cf =
        ((reference_liquidation_cf as u32) * (BPS_DENOMINATOR - LTV_BUFFER_BPS) as u32 / BPS_DENOMINATOR as u32) as u16;
    let reference_max_debt = value
        .saturating_mul(reference_max_cf as u128)
        .checked_div(BPS_DENOMINATOR as u128)
        .unwrap();

    assert!(terms.liquidation_cf_bps <= reference_liquidation_cf);
    assert!(terms.max_debt_nad <= reference_max_debt);
}

#[test]
fn pessimistic_max_debt_matches_v1_exact_values() {
    let reserve = 1_000_000_u128 * NAD as u128;
    let curve = ConcentratedRiskCurve {
        base_reserve_nad: reserve,
        quote_reserve_nad: reserve,
        center_price_nad: NAD as u128,
        peak_depth_nad: 0,
        fade_scale_nad: 0,
    };
    let direction = ConcentratedSwapDirection::BaseToQuote;

    let terms = pessimistic_max_debt_on_curve_nad(reserve, curve, direction, 0).unwrap();
    assert_eq!(terms.liquidation_cf_bps, 8_500);
    assert_eq!(terms.max_cf_bps, 8_075);
    assert_eq!(terms.max_debt_nad, 403_750_u128 * NAD as u128);

    let terms = pessimistic_max_debt_on_curve_nad(reserve / 2, curve, direction, 0).unwrap();
    assert_eq!(terms.max_cf_bps, 8_075);
    assert_eq!(terms.max_debt_nad, 269_166_666_666_666);

    let terms = pessimistic_max_debt_on_curve_nad(reserve / 2, curve, direction, 200_000_u128 * NAD as u128).unwrap();
    assert_eq!(terms.max_cf_bps, 6_514);
    assert_eq!(terms.max_debt_nad, 217_133_333_333_333);
}

#[test]
fn concentrated_utilized_collateral_composition_is_conservative() {
    let center = NAD as u128;
    let peak_depth = 200 * NAD as u128;
    let fade_scale = NAD as u128 / 10;
    let direction = ConcentratedSwapDirection::BaseToQuote;
    let reserves = crate::math::concentrated_risk_reserves_at_price_q(
        center,
        1_000_000 * NAD as u128,
        direction,
        center,
        peak_depth,
        fade_scale,
    )
    .unwrap();
    let curve = ConcentratedRiskCurve {
        base_reserve_nad: reserves.base_reserve_nad,
        quote_reserve_nad: reserves.quote_reserve_nad,
        center_price_nad: center,
        peak_depth_nad: peak_depth,
        fade_scale_nad: fade_scale,
    };
    let existing_debt = 400_000 * NAD as u128;
    let exact_out_input = curve.exact_out(existing_debt, direction).unwrap();
    let risk_utilized = concentrated_quote_exact_out_input_lower_bound(
        curve.base_reserve_nad,
        curve.quote_reserve_nad,
        existing_debt,
        direction,
        curve.center_price_nad,
        curve.peak_depth_nad,
        curve.fade_scale_nad,
    )
    .unwrap();

    // Binary-search the smallest input whose conservative exact-input quote
    // covers existing debt. This is an independent reference for the
    // exact-out/exact-in composition boundary.
    let mut low = 0_u128;
    let mut high = exact_out_input;
    while low < high {
        let midpoint = low + (high - low) / 2;
        if curve.exact_in(midpoint, direction).unwrap() >= existing_debt {
            high = midpoint;
        } else {
            low = midpoint + 1;
        }
    }
    let reference_utilized = low;
    assert!(risk_utilized <= reference_utilized);
    assert!(reference_utilized <= exact_out_input);

    let collateral = 200_000 * NAD as u128;
    let terms = pessimistic_max_debt_on_curve_nad(collateral, curve, direction, existing_debt).unwrap();
    let reference_max_total = curve.exact_in(reference_utilized + collateral, direction).unwrap();
    let reference_user_max = reference_max_total.saturating_sub(existing_debt);
    let value = curve.exact_in(collateral, direction).unwrap();
    let reference_liquidation_cf = reference_user_max
        .saturating_mul(BPS_DENOMINATOR as u128)
        .checked_div(value)
        .unwrap_or(0)
        .min(MAX_COLLATERAL_FACTOR_BPS as u128) as u16;
    let reference_max_cf =
        ((reference_liquidation_cf as u32) * (BPS_DENOMINATOR - LTV_BUFFER_BPS) as u32 / BPS_DENOMINATOR as u32) as u16;
    let reference_max_debt = value
        .saturating_mul(reference_max_cf as u128)
        .checked_div(BPS_DENOMINATOR as u128)
        .unwrap();

    assert!(terms.liquidation_cf_bps <= reference_liquidation_cf);
    assert!(terms.max_debt_nad <= reference_max_debt);
}

#[test]
fn risk_composition_uses_proven_inverse_bounds_across_hybrid_regions() {
    let reserve = 1_000_000_000_000_u128;
    for (peak_depth, fade_scale) in [
        (2 * NAD as u128, 10),
        (200 * NAD as u128, NAD as u128 / 10),
        (
            crate::math::CONCENTRATED_MAX_PEAK_DEPTH_NAD,
            crate::math::CONCENTRATED_MAX_FADE_SCALE_NAD,
        ),
    ] {
        let curve = ConcentratedRiskCurve {
            base_reserve_nad: reserve,
            quote_reserve_nad: reserve,
            center_price_nad: NAD as u128,
            peak_depth_nad: peak_depth,
            fade_scale_nad: fade_scale,
        };
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            for debt in [reserve / 100, reserve * 40 / 100, reserve * 80 / 100] {
                assert_risk_composition_is_conservative(curve, direction, debt, reserve / 10);
            }
        }
    }

    // Both one-sided CPMM tails and both restoring directions.
    let scarce = 100_000_000_000_u128;
    let abundant = 4_000_000_000_000_u128;
    for (base, quote) in [(abundant, scarce), (scarce, abundant)] {
        let curve = ConcentratedRiskCurve {
            base_reserve_nad: base,
            quote_reserve_nad: quote,
            center_price_nad: NAD as u128,
            peak_depth_nad: 200 * NAD as u128,
            fade_scale_nad: NAD as u128 / 10,
        };
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            let (input_reserve, output_reserve) = match direction {
                ConcentratedSwapDirection::BaseToQuote => (curve.base_reserve_nad, curve.quote_reserve_nad),
                ConcentratedSwapDirection::QuoteToBase => (curve.quote_reserve_nad, curve.base_reserve_nad),
            };
            assert_risk_composition_is_conservative(curve, direction, output_reserve / 2, input_reserve / 20);
        }
    }
}
