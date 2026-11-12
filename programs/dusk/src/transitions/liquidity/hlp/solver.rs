//! Pure math for the hedged-LP within-swap tracking solver.
//!
//! A 2x-leveraged constant-product LP tracks its deposit asset only in the
//! continuous-rebalancing limit. A single discrete swap of price ratio `r`
//! leaves a tracking gap of `E0 * (sqrt(r) - 1)^2`. That gap can be removed by
//! pre-positioning the vault before the swap with a `Δpre = E0 * (sqrt(r) - 1)`
//! leverage adjustment and finishing with the usual post-swap rebalance.
//!
//! These functions are invariant-independent numeraire building blocks and
//! analytic references. Production active-hLP swaps use the shared bounded
//! joint lifecycle solver for CPMM and concentration; they do not authorize a
//! state transition from this one-sided closed-form expression.
//!
//! All ratios/amounts are NAD fixed point (`NAD == 1.0`).

use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

/// The hLP's srLP inventory and opposite-asset debt, all valued in the target
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

/// Convenience helper that derives and allocates the ideal curve-aware hLP
/// adjustment in one call.

/// Discrete within-swap tracking loss `E0 * abs(sqrt(r) - 1)^2`, in NAD.

/// Closed-form pre-adjustment magnitude `|E0 * (sqrt(r) - 1)|`, in NAD, plus
/// whether it is a lever-up (`r > 1`) or a deleverage (`r < 1`). This is an
/// analytic CPMM counterfactual retained for theorem and regression tests; the
/// applied-curve predictor does not use it as a production seed.

#[cfg(test)]
mod tests {
    include!("../../../tests/transitions/liquidity_hlp_solver.rs");
}

#[cfg(test)]
include!("../../../tests/support/hlp_solver.rs");
