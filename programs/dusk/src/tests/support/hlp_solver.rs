use crate::constants::NAD;

use crate::math::{isqrt, mul_div_u128, ratio_lte_full_width, sqrt_ratio_nad};

/// A self-financing synthetic liquidity change, valued in the target asset's
/// NAD numeraire.
///
/// Positive values add proportional srLP inventory and borrow the same total
/// value. Negative values remove proportional inventory and repay debt. The
/// two inventory deltas always sum exactly to `total_liquidity_value_nad`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlpProportionalAdjustmentNad {
    pub total_liquidity_value_nad: i128,
    pub target_inventory_value_delta_nad: i128,
    pub opposite_inventory_value_delta_nad: i128,
    pub debt_value_delta_nad: i128,
}

fn signed_with_direction(magnitude: u128, negative: bool) -> Result<i128> {
    signed_from_magnitude(magnitude, negative)
}

/// Splits a signed total liquidity change according to the hLP's *actual*
/// target/opposite inventory value weights.
///
/// The target leg is rounded toward zero and the opposite leg receives the
/// remainder, so the two legs sum exactly to the requested total. This avoids
/// the 50/50-value assumption, which is only valid at a curve state whose two
/// reserve values happen to be equal.
pub fn allocate_hlp_proportional_adjustment_nad(
    values: HlpInventoryValuesNad,
    total_liquidity_value_nad: i128,
) -> Result<HlpProportionalAdjustmentNad> {
    if total_liquidity_value_nad == 0 {
        return Ok(HlpProportionalAdjustmentNad::default());
    }

    let collateral_value = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(collateral_value > 0, ErrorCode::DenominatorOverflow);

    let negative = total_liquidity_value_nad < 0;
    let total_magnitude = total_liquidity_value_nad.unsigned_abs();
    let target_magnitude = mul_div_u128(total_magnitude, values.target_inventory_value_nad, collateral_value)?;
    let opposite_magnitude = total_magnitude
        .checked_sub(target_magnitude)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let target_delta = signed_with_direction(target_magnitude, negative)?;
    let opposite_delta = signed_with_direction(opposite_magnitude, negative)?;

    Ok(HlpProportionalAdjustmentNad {
        total_liquidity_value_nad,
        target_inventory_value_delta_nad: target_delta,
        opposite_inventory_value_delta_nad: opposite_delta,
        debt_value_delta_nad: total_liquidity_value_nad,
    })
}

/// Convenience helper that derives and allocates the ideal curve-aware hLP
/// adjustment in one call.
pub fn ideal_hlp_rebalance_nad(values: HlpInventoryValuesNad) -> Result<HlpProportionalAdjustmentNad> {
    let exposure = hlp_opposite_exposure_nad(values)?;
    if exposure == 0 {
        return Ok(HlpProportionalAdjustmentNad::default());
    }
    require!(values.target_inventory_value_nad > 0, ErrorCode::DenominatorOverflow);

    // With e = O - D, the ideal proportional correction simplifies to:
    // ΔT=e, ΔO=eO/T, ΔD=e+ΔO. Rounding ΔO toward zero leaves at most one
    // target-value NAD atom of opposite exposure.
    let negative = exposure < 0;
    let opposite_magnitude = mul_div_u128(
        exposure.unsigned_abs(),
        values.opposite_inventory_value_nad,
        values.target_inventory_value_nad,
    )?;
    let target_delta = signed_with_direction(exposure.unsigned_abs(), negative)?;
    let opposite_delta = signed_with_direction(opposite_magnitude, negative)?;
    let total = target_delta
        .checked_add(opposite_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(HlpProportionalAdjustmentNad {
        total_liquidity_value_nad: total,
        target_inventory_value_delta_nad: target_delta,
        opposite_inventory_value_delta_nad: opposite_delta,
        debt_value_delta_nad: total,
    })
}

/// Discrete within-swap tracking loss `E0 * abs(sqrt(r) - 1)^2`, in NAD.
pub fn tracking_loss_nad(equity_nad: u128, r_nad: u128) -> Result<u128> {
    if equity_nad == 0 || r_nad == NAD as u128 {
        return Ok(0);
    }
    let s = sqrt_ratio_nad(r_nad)?;
    let gap = s.abs_diff(NAD as u128);
    // equity * gap^2 / NAD^2
    equity_nad
        .checked_mul(gap)
        .and_then(|value| value.checked_div(NAD as u128))
        .and_then(|value| value.checked_mul(gap))
        .and_then(|value| value.checked_div(NAD as u128))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

/// Closed-form pre-adjustment magnitude `|E0 * (sqrt(r) - 1)|`, in NAD, plus
/// whether it is a lever-up (`r > 1`) or a deleverage (`r < 1`). This is an
/// analytic CPMM counterfactual retained for theorem and regression tests; the
/// applied-curve predictor does not use it as a production seed.
pub fn closed_form_pre_adjustment_nad(equity_nad: u128, r_nad: u128) -> Result<(u128, bool)> {
    let s = sqrt_ratio_nad(r_nad)?;
    let nad = NAD as u128;
    if s >= nad {
        let gap = s - nad;
        let amount = equity_nad
            .checked_mul(gap)
            .and_then(|value| value.checked_div(nad))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok((amount, true))
    } else {
        let gap = nad - s;
        let amount = equity_nad
            .checked_mul(gap)
            .and_then(|value| value.checked_div(nad))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok((amount, false))
    }
}
