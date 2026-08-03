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
//! point `a = E0 * (sqrt(r(a)) - 1)`, solved with bounded bisection over the
//! real swap simulator. These functions are the numeraire-only building blocks
//! (loss estimate, closed-form guess, root finder); the market-state
//! orchestration runs only when the estimated tracking loss exceeds the
//! configured threshold.
//!
//! All ratios/amounts are NAD fixed point (`NAD == 1.0`).

use anchor_lang::prelude::*;

use crate::constants::NAD;
use crate::errors::ErrorCode;

#[allow(clippy::assign_op_pattern, clippy::manual_div_ceil)]
mod exposure_wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U256(4);
    }
}

use exposure_wide::U256;

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

fn signed_from_magnitude(magnitude: U256, negative: bool) -> Result<i128> {
    let negative_limit = U256::from(1_u128 << 127);
    let positive_limit = U256::from(i128::MAX as u128);
    let limit = if negative { negative_limit } else { positive_limit };
    require!(magnitude <= limit, ErrorCode::MarketMathOverflow);

    let magnitude = magnitude.as_u128();
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

fn signed_difference(left: u128, right: u128) -> Result<i128> {
    if left >= right {
        signed_from_magnitude(U256::from(left - right), false)
    } else {
        signed_from_magnitude(U256::from(right - left), true)
    }
}

fn signed_with_direction(magnitude: u128, negative: bool) -> Result<i128> {
    signed_from_magnitude(U256::from(magnitude), negative)
}

fn mul_div_u128_full_width(value: u128, numerator: u128, denominator: U256) -> Result<u128> {
    require!(!denominator.is_zero(), ErrorCode::DenominatorOverflow);
    let result = U256::from(value)
        .checked_mul(U256::from(numerator))
        .ok_or(ErrorCode::MarketMathOverflow)?
        / denominator;
    require!(result <= U256::from(u128::MAX), ErrorCode::MarketMathOverflow);
    Ok(result.as_u128())
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
    let left = U256::from(left_numerator)
        .checked_mul(U256::from(right_denominator))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let right = U256::from(right_numerator)
        .checked_mul(U256::from(left_denominator))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(left <= right)
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
    signed_difference(values.opposite_inventory_value_nad, values.debt_value_nad)
}

/// Ideal total value of a self-financing *proportional* liquidity adjustment.
///
/// Let `T` and `O` be the actual target/opposite yLP inventory values, `D` the
/// opposite debt value, and `C = T + O`. Adding total liquidity value `L`
/// changes the inventory by `(L*T/C, L*O/C)` and changes debt by `L`. The
/// post-adjustment opposite exposure is:
///
/// ```text
/// O + L*O/C - (D + L) = (O - D) - L*T/C
/// ```
///
/// Setting it to zero gives `L = (O - D) * C / T`. Positive `L` leverages up;
/// negative `L` deleverages. When `T == O`, this reduces exactly to the legacy
/// CPMM expression `C - 2*D`.
///
/// Integer division rounds the magnitude toward zero. Combined with
/// [`allocate_hlp_proportional_adjustment_nad`], the remaining exposure is at
/// most one NAD unit before feasibility caps.
pub fn ideal_hlp_proportional_adjustment_nad(values: HlpInventoryValuesNad) -> Result<i128> {
    let (residual_magnitude, negative) = if values.opposite_inventory_value_nad >= values.debt_value_nad {
        (values.opposite_inventory_value_nad - values.debt_value_nad, false)
    } else {
        (values.debt_value_nad - values.opposite_inventory_value_nad, true)
    };
    if residual_magnitude == 0 {
        return Ok(0);
    }
    require!(values.target_inventory_value_nad > 0, ErrorCode::DenominatorOverflow);

    let collateral_value = U256::from(values.target_inventory_value_nad)
        .checked_add(U256::from(values.opposite_inventory_value_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let adjustment_magnitude = U256::from(residual_magnitude)
        .checked_mul(collateral_value)
        .ok_or(ErrorCode::MarketMathOverflow)?
        / U256::from(values.target_inventory_value_nad);
    signed_from_magnitude(adjustment_magnitude, negative)
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

    let collateral_value = U256::from(values.target_inventory_value_nad)
        .checked_add(U256::from(values.opposite_inventory_value_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(!collateral_value.is_zero(), ErrorCode::DenominatorOverflow);

    let negative = total_liquidity_value_nad < 0;
    let total_magnitude = total_liquidity_value_nad.unsigned_abs();
    let target_magnitude =
        mul_div_u128_full_width(total_magnitude, values.target_inventory_value_nad, collateral_value)?;
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
    let total = ideal_hlp_proportional_adjustment_nad(values)?;
    allocate_hlp_proportional_adjustment_nad(values, total)
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
/// initial bisection guess; the true value is solved against the simulator
/// because the synthetic deepening makes `r` endogenous.
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

/// Bounded bisection for a monotonically non-decreasing residual `f` over
/// `[lo, hi]`, returning the smallest `x` with `f(x) >= 0` to tolerance, within
/// `max_iters`. `f` returns the signed residual (negative below the root).
/// Used to solve the endogenous-`r` pre-adjustment fixed point against the real
/// swap simulator without unbounded compute.
pub fn bisect<F>(mut lo: u128, mut hi: u128, max_iters: u32, mut f: F) -> Result<u128>
where
    F: FnMut(u128) -> Result<i128>,
{
    require!(hi >= lo, ErrorCode::MarketMathOverflow);
    for _ in 0..max_iters {
        if hi <= lo + 1 {
            break;
        }
        let mid = lo + (hi - lo) / 2;
        if f(mid)? >= 0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(hi)
}

#[cfg(test)]
mod tests {
    include!("../tests/math/hlp_solver.rs");
}
