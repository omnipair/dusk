//! Pure math for the hedged-LP within-swap tracking solver (Phase 2).
//!
//! A 2x-leveraged constant-product LP tracks its deposit asset only in the
//! continuous-rebalancing limit. A single discrete swap of price ratio `r`
//! leaves a tracking gap of `E0 * (sqrt(r) - 1)^2`. That gap can be removed by
//! pre-positioning the vault before the swap with a `Δpre = E0 * (sqrt(r) - 1)`
//! leverage adjustment and finishing with the usual post-swap rebalance.
//!
//! In Omnipair the pre-adjustment is a *price-neutral synthetic deepening*, so
//! it changes the realized `r` (endogenous): the production `Δpre` is the fixed
//! point `a = E0 * (sqrt(r(a)) - 1)`, approximated by the protocol-fixed
//! three-evaluation safeguarded secant predictor over the real swap simulator.
//! These functions are the numeraire-only building blocks (loss estimate and
//! closed-form seed); market-state orchestration runs only when estimated
//! tracking loss exceeds the configured threshold.
//!
//! All ratios/amounts are NAD fixed point (`NAD == 1.0`).

use anchor_lang::prelude::*;

use crate::constants::NAD;
use crate::errors::ErrorCode;

/// The hLP's yLP inventory and opposite-asset debt, all valued in the target
/// asset's NAD numeraire at the AMM curve's actual marginal price.
///
/// Keeping valuation outside this module makes the exposure math independent
/// of the invariant: CPMM, CONCENTRATED, and future curves only need to supply the three
/// values at their own marginal price.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlpInventoryValuesNad {
    pub target_inventory_value_nad: u128,
    pub opposite_inventory_value_nad: u128,
    pub debt_value_nad: u128,
}

/// A self-financing synthetic liquidity change, valued in the target asset's
/// NAD numeraire.
///
/// Positive values add proportional yLP inventory and borrow the same total
/// value. Negative values remove proportional inventory and repay debt. The
/// two inventory deltas always sum exactly to `total_liquidity_value_nad`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlpProportionalAdjustmentNad {
    pub total_liquidity_value_nad: i128,
    pub target_inventory_value_delta_nad: i128,
    pub opposite_inventory_value_delta_nad: i128,
    pub debt_value_delta_nad: i128,
}

fn signed_from_magnitude(magnitude: u128, negative: bool) -> Result<i128> {
    let negative_limit = 1_u128 << 127;
    let positive_limit = i128::MAX as u128;
    let limit = if negative { negative_limit } else { positive_limit };
    require!(magnitude <= limit, ErrorCode::MarketMathOverflow);

    if !negative {
        return i128::try_from(magnitude).map_err(|_| ErrorCode::MarketMathOverflow.into());
    }
    if magnitude == 1_u128 << 127 {
        return Ok(i128::MIN);
    }
    let magnitude = i128::try_from(magnitude).map_err(|_| ErrorCode::MarketMathOverflow)?;
    magnitude
        .checked_neg()
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn signed_with_direction(magnitude: u128, negative: bool) -> Result<i128> {
    signed_from_magnitude(magnitude, negative)
}

/// Exact `floor(value * numerator / denominator)` with a checked u128 result.
///
/// The ordinary path is one native multiply and divide. If the product is
/// wider than u128, binary quotient/remainder accumulation keeps only a
/// denominator-bounded remainder and fails as soon as the quotient itself no
/// longer fits. No software big-integer limbs are used.
pub(crate) fn mul_div_rem_u128(value: u128, numerator: u128, denominator: u128) -> Result<(u128, u128)> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    if value == 0 || numerator == 0 {
        return Ok((0, 0));
    }
    if let Some(product) = value.checked_mul(numerator) {
        return Ok((product / denominator, product % denominator));
    }

    let whole = value / denominator;
    let base_quotient = whole.checked_mul(numerator).ok_or(ErrorCode::MarketMathOverflow)?;
    let addend = value % denominator;
    let mut quotient = 0_u128;
    let mut remainder = 0_u128;

    for bit in (0..128).rev() {
        let carry = if remainder >= denominator - remainder {
            remainder -= denominator - remainder;
            1_u128
        } else {
            remainder += remainder;
            0_u128
        };
        quotient = quotient
            .checked_mul(2)
            .and_then(|result| result.checked_add(carry))
            .ok_or(ErrorCode::MarketMathOverflow)?;

        if (numerator >> bit) & 1 == 1 {
            let carry = if remainder >= denominator - addend {
                remainder -= denominator - addend;
                1_u128
            } else {
                remainder += addend;
                0_u128
            };
            quotient = quotient.checked_add(carry).ok_or(ErrorCode::MarketMathOverflow)?;
        }
    }
    let quotient = base_quotient
        .checked_add(quotient)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((quotient, remainder))
}

pub(crate) fn mul_div_u128(value: u128, numerator: u128, denominator: u128) -> Result<u128> {
    Ok(mul_div_rem_u128(value, numerator, denominator)?.0)
}

pub(crate) fn mul_div_ceil_u128(value: u128, numerator: u128, denominator: u128) -> Result<u128> {
    let (quotient, remainder) = mul_div_rem_u128(value, numerator, denominator)?;
    quotient
        .checked_add(u128::from(remainder != 0))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

/// Compare two unsigned ratios without overflowing their u128 cross-products.
/// hLP admission uses this to ensure a granularity-limited top-up cannot
/// increase residual exposure per share or per unit of NAV.
pub(crate) fn ratio_lte_full_width(
    left_numerator: u128,
    left_denominator: u128,
    right_numerator: u128,
    right_denominator: u128,
) -> Result<bool> {
    require!(
        left_denominator > 0 && right_denominator > 0,
        ErrorCode::DenominatorOverflow
    );
    if let (Some(left), Some(right)) = (
        left_numerator.checked_mul(right_denominator),
        right_numerator.checked_mul(left_denominator),
    ) {
        return Ok(left <= right);
    }

    // Continued fractions compare the exact ratios without cross-products.
    // Each reciprocal step strictly reduces a denominator, so this is bounded
    // by the Euclidean algorithm rather than an open-ended numeric search.
    let (mut left_n, mut left_d) = (left_numerator, left_denominator);
    let (mut right_n, mut right_d) = (right_numerator, right_denominator);
    let mut reversed = false;
    loop {
        let left_whole = left_n / left_d;
        let right_whole = right_n / right_d;
        if left_whole != right_whole {
            return Ok(if reversed {
                left_whole > right_whole
            } else {
                left_whole < right_whole
            });
        }

        let left_remainder = left_n % left_d;
        let right_remainder = right_n % right_d;
        match (left_remainder == 0, right_remainder == 0) {
            (true, true) => return Ok(true),
            (true, false) => return Ok(!reversed),
            (false, true) => return Ok(reversed),
            (false, false) => {
                (left_n, left_d) = (left_d, left_remainder);
                (right_n, right_d) = (right_d, right_remainder);
                reversed = !reversed;
            }
        }
    }
}

/// Local hLP exposure to the opposite asset, in target-value NAD:
///
/// ```text
/// opposite exposure = opposite inventory value - opposite debt value
/// ```
///
/// For a small relative move in the opposite asset's price, the target leg is
/// constant in the chosen numeraire while both the opposite inventory and its
/// debt move with price. The hLP is therefore delta-neutral exactly when their
/// values are equal.
pub fn hlp_opposite_exposure_nad(values: HlpInventoryValuesNad) -> Result<i128> {
    if values.opposite_inventory_value_nad >= values.debt_value_nad {
        signed_from_magnitude(values.opposite_inventory_value_nad - values.debt_value_nad, false)
    } else {
        signed_from_magnitude(values.debt_value_nad - values.opposite_inventory_value_nad, true)
    }
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

/// Integer square root (floor), Newton's method on u128.
pub fn isqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    // Initial guess: 2^(ceil(bits/2)).
    let mut x = 1u128 << ((128 - value.leading_zeros()).div_ceil(2));
    loop {
        let next = (x + value / x) / 2;
        if next >= x {
            return x;
        }
        x = next;
    }
}

/// `sqrt(r)` in NAD, where `r_nad = r * NAD`. Returns `sqrt(r) * NAD`.
pub fn sqrt_ratio_nad(r_nad: u128) -> Result<u128> {
    // sqrt(r) * NAD = sqrt(r_nad * NAD).
    let scaled = r_nad.checked_mul(NAD as u128).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(isqrt(scaled))
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
/// whether it is a lever-up (`r > 1`) or a deleverage (`r < 1`). Used as the
/// initial safeguarded-secant seed; the accepted value is checked against the
/// simulator because the synthetic deepening makes `r` endogenous.
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

#[cfg(test)]
mod tests {
    include!("../tests/math/hlp_solver.rs");
}
