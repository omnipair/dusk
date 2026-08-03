//! Conservative reserve reconstruction for Dusk Concentrated AMM risk views.
//!
//! The invariant is homogeneous: scaling both reserves scales invariant `D`
//! and balanced-equivalent `Q`, while leaving marginal price unchanged. Risk
//! snapshots therefore solve a dimensionless shape at the requested price and
//! then scale that shape to a deliberately reduced `Q` budget.
//!
//! Inside the protocol shoulder, balance factor `b = 4*x*y/D^2` and
//! `delta = 1-b` give the normalized reserve sum directly:
//! ```text
//! t = (x+y)/D
//! t = 1 + delta*(imbalance_scale+delta)^2
//!         / (2*peak_depth*b*imbalance_scale^2)
//! ```
//! The inner coordinates follow from their sum and product. At
//! `delta = imbalance_scale`, the production invariant exposes the canonical
//! shoulder and its two one-sided marginal prices. Targets in that marginal
//! kink map to the shoulder itself; targets beyond it reconstruct directly on
//! the exact CPMM continuation. This module never nests the public quote or
//! invariant solvers inside the risk-price search.

use anchor_lang::prelude::*;

use crate::{constants::NAD, errors::ErrorCode};

use super::{
    concentrated_hybrid_shoulder_from_d, concentrated_marginal_price_from_common, ConcentratedHybridShoulder,
    ConcentratedSwapDirection, CONCENTRATED_MAX_IMBALANCE_SCALE_NAD, CONCENTRATED_MAX_PEAK_DEPTH_NAD,
};

#[allow(clippy::assign_op_pattern, clippy::manual_div_ceil)]
mod wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U256(4);
    }

    construct_uint! {
        pub struct U512(8);
    }
}

use wide::{U256, U512};

const SHAPE_SCALE: u128 = 1_000_000_000_000_000_000;
const PPM_DENOMINATOR: u128 = 1_000_000;

/// Directed reserve rounding gives up this much balanced-equivalent depth.
pub(crate) const CONCENTRATED_RISK_Q_SAFETY_PPM: u128 = 25;
/// Covers shape search, integer coordinates, and execution solver margins.
pub(crate) const CONCENTRATED_RISK_PRICE_SAFETY_PPM: u128 = 500;
pub(crate) const CONCENTRATED_RISK_PRICE_MAX_ITERS: usize = 32;
pub(crate) const CONCENTRATED_RISK_SQRT_MAX_ITERS: usize = 16;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static SHAPE_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
    static SQRT_ITERATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_risk_shape_counters() {
    SHAPE_EVALUATIONS.with(|count| count.set(0));
    SQRT_ITERATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn risk_shape_counters() -> (usize, usize) {
    (SHAPE_EVALUATIONS.with(Cell::get), SQRT_ITERATIONS.with(Cell::get))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConcentratedRiskReserves {
    pub base_reserve_nad: u128,
    pub quote_reserve_nad: u128,
}

pub(crate) fn scale_concentrated_risk_reserves_floor(
    reserves: ConcentratedRiskReserves,
    numerator: u128,
    denominator: u128,
) -> Result<ConcentratedRiskReserves> {
    require!(
        numerator > 0 && denominator > 0 && numerator <= denominator,
        ErrorCode::InvalidArgument
    );
    if numerator == denominator {
        return Ok(reserves);
    }
    let scaled = ConcentratedRiskReserves {
        base_reserve_nad: mul_div_floor(reserves.base_reserve_nad, numerator, denominator)?,
        quote_reserve_nad: mul_div_floor(reserves.quote_reserve_nad, numerator, denominator)?,
    };
    require!(
        scaled.base_reserve_nad > 0 && scaled.quote_reserve_nad > 0,
        ErrorCode::InsufficientLiquidity
    );
    Ok(scaled)
}

fn u256_to_u128(value: U256) -> Result<u128> {
    require!(value <= U256::from(u128::MAX), ErrorCode::InvariantOverflow);
    Ok(value.as_u128())
}

fn u512_to_u128(value: U512) -> Result<u128> {
    require!(value <= U512::from(u128::MAX), ErrorCode::InvariantOverflow);
    Ok(value.as_u128())
}

fn mul_div_floor(a: u128, b: u128, denominator: u128) -> Result<u128> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    u256_to_u128(
        U256::from(a)
            .checked_mul(U256::from(b))
            .ok_or(ErrorCode::InvariantOverflow)?
            / U256::from(denominator),
    )
}

fn mul_div_ceil(a: u128, b: u128, denominator: u128) -> Result<u128> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    let numerator = U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let denominator = U256::from(denominator);
    let value = if numerator.is_zero() {
        U256::zero()
    } else {
        (numerator - U256::one()) / denominator + U256::one()
    };
    u256_to_u128(value)
}

fn sqrt_floor_u512_to_u128(value: U512) -> Result<u128> {
    if value.is_zero() {
        return Ok(0);
    }
    let max_root = U512::from(u128::MAX);
    require!(
        value <= max_root.checked_mul(max_root).ok_or(ErrorCode::InvariantOverflow)?,
        ErrorCode::InvariantOverflow
    );
    let mut root = U512::one() << value.bits().div_ceil(2);
    for _ in 0..CONCENTRATED_RISK_SQRT_MAX_ITERS {
        #[cfg(test)]
        SQRT_ITERATIONS.with(|count| count.set(count.get() + 1));
        let next = root.checked_add(value / root).ok_or(ErrorCode::InvariantOverflow)? >> 1;
        if next >= root {
            return u512_to_u128(root);
        }
        root = next;
    }
    err!(ErrorCode::InvariantOverflow)
}

fn sqrt_ceil_u512_to_u128(value: U512) -> Result<u128> {
    let floor = sqrt_floor_u512_to_u128(value)?;
    let square = U512::from(floor)
        .checked_mul(U512::from(floor))
        .ok_or(ErrorCode::InvariantOverflow)?;
    if square == value {
        Ok(floor)
    } else {
        floor.checked_add(1).ok_or_else(|| ErrorCode::InvariantOverflow.into())
    }
}

fn sqrt_ratio_floor_u512_to_u128(numerator: U512, denominator: U512) -> Result<u128> {
    require!(!denominator.is_zero(), ErrorCode::DenominatorOverflow);
    sqrt_floor_u512_to_u128(numerator / denominator)
}

fn sqrt_ratio_ceil_u512_to_u128(numerator: U512, denominator: U512) -> Result<u128> {
    require!(!denominator.is_zero(), ErrorCode::DenominatorOverflow);
    let quotient = if numerator.is_zero() {
        U512::zero()
    } else {
        (numerator - U512::one()) / denominator + U512::one()
    };
    sqrt_ceil_u512_to_u128(quotient)
}

fn validate_inputs(
    target_price_nad: u128,
    balanced_equivalent_q_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<()> {
    require!(
        target_price_nad > 0 && balanced_equivalent_q_nad > 0 && center_price_nad > 0,
        ErrorCode::InvalidArgument
    );
    require!(
        peak_depth_nad <= CONCENTRATED_MAX_PEAK_DEPTH_NAD,
        ErrorCode::InvalidArgument
    );
    if peak_depth_nad == 0 || imbalance_scale_nad == 0 {
        require!(
            peak_depth_nad == 0 && imbalance_scale_nad == 0,
            ErrorCode::InvalidArgument
        );
    } else {
        require!(
            imbalance_scale_nad <= CONCENTRATED_MAX_IMBALANCE_SCALE_NAD,
            ErrorCode::InvalidArgument
        );
    }
    Ok(())
}

fn conservative_q(balanced_equivalent_q_nad: u128) -> Result<u128> {
    let kept_ppm = PPM_DENOMINATOR
        .checked_sub(CONCENTRATED_RISK_Q_SAFETY_PPM)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let q = mul_div_floor(balanced_equivalent_q_nad, kept_ppm, PPM_DENOMINATOR)?;
    require!(q > 0, ErrorCode::InsufficientLiquidity);
    Ok(q)
}

fn cpmm_reserves_at_price_q(
    target_price_nad: u128,
    balanced_equivalent_q_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
) -> Result<ConcentratedRiskReserves> {
    let q = conservative_q(balanced_equivalent_q_nad)?;
    let q_squared = U512::from(q)
        .checked_mul(U512::from(q))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let base_squared = q_squared
        .checked_mul(U512::from(NAD))
        .ok_or(ErrorCode::InvariantOverflow)?
        / U512::from(target_price_nad);
    let quote_squared = q_squared
        .checked_mul(U512::from(target_price_nad))
        .ok_or(ErrorCode::InvariantOverflow)?
        / U512::from(NAD);
    let (base_reserve_nad, quote_reserve_nad) = match collateral_to_debt {
        ConcentratedSwapDirection::BaseToQuote => (
            sqrt_ceil_u512_to_u128(base_squared)?,
            sqrt_floor_u512_to_u128(quote_squared)?,
        ),
        ConcentratedSwapDirection::QuoteToBase => (
            sqrt_floor_u512_to_u128(base_squared)?,
            sqrt_ceil_u512_to_u128(quote_squared)?,
        ),
    };
    require!(
        base_reserve_nad > 0 && quote_reserve_nad > 0,
        ErrorCode::InsufficientLiquidity
    );
    Ok(ConcentratedRiskReserves {
        base_reserve_nad,
        quote_reserve_nad,
    })
}

fn common_coordinates_to_risk_reserves(
    base_common: u128,
    quote_common: u128,
    center_price_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
) -> Result<ConcentratedRiskReserves> {
    let (base_reserve_nad, quote_reserve_nad) = match collateral_to_debt {
        ConcentratedSwapDirection::BaseToQuote => {
            (mul_div_ceil(base_common, NAD as u128, center_price_nad)?, quote_common)
        }
        ConcentratedSwapDirection::QuoteToBase => {
            (mul_div_floor(base_common, NAD as u128, center_price_nad)?, quote_common)
        }
    };
    require!(
        base_reserve_nad > 0 && quote_reserve_nad > 0,
        ErrorCode::InsufficientLiquidity
    );
    Ok(ConcentratedRiskReserves {
        base_reserve_nad,
        quote_reserve_nad,
    })
}

fn shoulder_risk_reserves(
    shoulder: ConcentratedHybridShoulder,
    below_center: bool,
    center_price_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
) -> Result<ConcentratedRiskReserves> {
    let (base_common, quote_common) = if below_center {
        (shoulder.high_common, shoulder.low_common)
    } else {
        (shoulder.low_common, shoulder.high_common)
    };
    common_coordinates_to_risk_reserves(base_common, quote_common, center_price_nad, collateral_to_debt)
}

/// Reconstructs a point on the exact CPMM continuation using the production
/// shoulder product. Coordinate rounding is directed toward a worse
/// collateral-to-debt execution price.
fn tail_risk_reserves(
    shoulder: ConcentratedHybridShoulder,
    low_marginal_nad: u128,
    below_center: bool,
    center_price_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
) -> Result<ConcentratedRiskReserves> {
    require!(
        low_marginal_nad > 0 && low_marginal_nad <= shoulder.tail_low_marginal_nad,
        ErrorCode::InvalidArgument
    );
    let product = U512::from(shoulder.tail_product_common);
    let low_numerator = product
        .checked_mul(U512::from(low_marginal_nad))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let high_numerator = product
        .checked_mul(U512::from(NAD))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let low_denominator = U512::from(NAD);
    let high_denominator = U512::from(low_marginal_nad);

    let base_is_low = !below_center;
    let base_rounds_up = collateral_to_debt == ConcentratedSwapDirection::BaseToQuote;
    let coordinate = |is_low: bool, rounds_up: bool| -> Result<u128> {
        let (numerator, denominator) = if is_low {
            (low_numerator, low_denominator)
        } else {
            (high_numerator, high_denominator)
        };
        if rounds_up {
            sqrt_ratio_ceil_u512_to_u128(numerator, denominator)
        } else {
            sqrt_ratio_floor_u512_to_u128(numerator, denominator)
        }
    };
    let base_common = coordinate(base_is_low, base_rounds_up)?;
    let quote_common = coordinate(!base_is_low, !base_rounds_up)?;
    common_coordinates_to_risk_reserves(base_common, quote_common, center_price_nad, collateral_to_debt)
}

/// Returns `(t, z)` at SHAPE_SCALE precision, where reserves in common
/// coordinates are proportional to `(t+z, t-z)`.
fn shape_coordinates(
    balance_factor_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(u128, u128)> {
    require!(
        balance_factor_nad > 0 && balance_factor_nad <= NAD as u128,
        ErrorCode::InvalidArgument
    );
    if balance_factor_nad == NAD as u128 {
        return Ok((SHAPE_SCALE, 0));
    }
    let delta = (NAD as u128) - balance_factor_nad;
    let scale_plus_delta = imbalance_scale_nad
        .checked_add(delta)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let numerator = U512::from(delta)
        .checked_mul(U512::from(scale_plus_delta))
        .and_then(|value| value.checked_mul(U512::from(scale_plus_delta)))
        .and_then(|value| value.checked_mul(U512::from(NAD)))
        .and_then(|value| value.checked_mul(U512::from(SHAPE_SCALE)))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let denominator = U512::from(2_u8)
        .checked_mul(U512::from(peak_depth_nad))
        .and_then(|value| value.checked_mul(U512::from(balance_factor_nad)))
        .and_then(|value| value.checked_mul(U512::from(imbalance_scale_nad)))
        .and_then(|value| value.checked_mul(U512::from(imbalance_scale_nad)))
        .ok_or(ErrorCode::InvariantOverflow)?;
    require!(!denominator.is_zero(), ErrorCode::DenominatorOverflow);
    let offset = u512_to_u128(numerator / denominator)?;
    let t = SHAPE_SCALE.checked_add(offset).ok_or(ErrorCode::InvariantOverflow)?;
    let t_squared = U512::from(t)
        .checked_mul(U512::from(t))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let product_term = U512::from(balance_factor_nad)
        .checked_mul(U512::from(SHAPE_SCALE))
        .and_then(|value| value.checked_mul(U512::from(SHAPE_SCALE)))
        .ok_or(ErrorCode::InvariantOverflow)?
        / U512::from(NAD);
    let z = sqrt_floor_u512_to_u128(
        t_squared
            .checked_sub(product_term)
            .ok_or(ErrorCode::InvariantOverflow)?,
    )?;
    Ok((t, z))
}

fn shape_marginal_nad(balance_factor_nad: u128, peak_depth_nad: u128, imbalance_scale_nad: u128) -> Result<u128> {
    #[cfg(test)]
    SHAPE_EVALUATIONS.with(|count| count.set(count.get() + 1));

    let (t, z) = shape_coordinates(balance_factor_nad, peak_depth_nad, imbalance_scale_nad)?;
    let x = t.checked_add(z).ok_or(ErrorCode::InvariantOverflow)?;
    let y = t.checked_sub(z).ok_or(ErrorCode::InvariantOverflow)?;
    require!(y > 0, ErrorCode::InsufficientLiquidity);
    concentrated_marginal_price_from_common(
        x,
        y,
        SHAPE_SCALE.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
        NAD as u128,
        peak_depth_nad,
        imbalance_scale_nad,
    )
}

/// Finds the central-branch balance factor whose low-side normalized marginal
/// brackets the target. Lower balance factor means farther from center; the
/// search is explicitly bounded by the protocol shoulder.
fn solve_balance_factor_nad(
    target_marginal_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    choose_away_endpoint: bool,
) -> Result<u128> {
    require!(
        target_marginal_nad > 0 && target_marginal_nad <= NAD as u128,
        ErrorCode::InvalidArgument
    );
    if target_marginal_nad == NAD as u128 {
        return Ok(NAD as u128);
    }

    let mut high = NAD as u128;
    let mut low = (NAD as u128)
        .checked_sub(imbalance_scale_nad)
        .ok_or(ErrorCode::InvariantOverflow)?;
    require!(low > 0, ErrorCode::InvalidArgument);

    for _ in 0..CONCENTRATED_RISK_PRICE_MAX_ITERS {
        if high - low <= 1 {
            break;
        }
        let midpoint = low + (high - low) / 2;
        if shape_marginal_nad(midpoint, peak_depth_nad, imbalance_scale_nad)? <= target_marginal_nad {
            low = midpoint;
        } else {
            high = midpoint;
        }
    }
    Ok(if choose_away_endpoint { low } else { high })
}

fn invariant_d_for_q(q_nad: u128, center_price_nad: u128) -> Result<u128> {
    let radicand = U512::from(q_nad)
        .checked_mul(U512::from(q_nad))
        .and_then(|value| value.checked_mul(U512::from(center_price_nad)))
        .and_then(|value| value.checked_mul(U512::from(4_u8)))
        .ok_or(ErrorCode::InvariantOverflow)?
        / U512::from(NAD);
    let d = sqrt_floor_u512_to_u128(radicand)?;
    require!(d > 0, ErrorCode::InsufficientLiquidity);
    Ok(d)
}

fn concentrated_reserves_at_price_q(
    target_price_nad: u128,
    balanced_equivalent_q_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<ConcentratedRiskReserves> {
    let q = conservative_q(balanced_equivalent_q_nad)?;
    let d = invariant_d_for_q(q, center_price_nad)?;
    let shoulder = concentrated_hybrid_shoulder_from_d(d, peak_depth_nad, imbalance_scale_nad)?;

    let below_center = target_price_nad < center_price_nad
        || (target_price_nad == center_price_nad && collateral_to_debt == ConcentratedSwapDirection::BaseToQuote);
    let low_marginal_nad = if below_center {
        mul_div_floor(target_price_nad, NAD as u128, center_price_nad)?
    } else {
        mul_div_floor(center_price_nad, NAD as u128, target_price_nad)?
    };
    require!(low_marginal_nad > 0, ErrorCode::InvalidArgument);
    let move_away_from_center = matches!(
        (below_center, collateral_to_debt),
        (true, ConcentratedSwapDirection::BaseToQuote) | (false, ConcentratedSwapDirection::QuoteToBase)
    );
    let directed_marginal_nad = if move_away_from_center {
        mul_div_floor(
            low_marginal_nad,
            PPM_DENOMINATOR
                .checked_sub(CONCENTRATED_RISK_PRICE_SAFETY_PPM)
                .ok_or(ErrorCode::InvariantOverflow)?,
            PPM_DENOMINATOR,
        )?
        .max(1)
    } else {
        mul_div_ceil(
            low_marginal_nad,
            PPM_DENOMINATOR
                .checked_add(CONCENTRATED_RISK_PRICE_SAFETY_PPM)
                .ok_or(ErrorCode::InvariantOverflow)?,
            PPM_DENOMINATOR,
        )?
        .min(NAD as u128)
    };

    // The invariant is value-continuous but deliberately not tangent at the
    // shoulder. No reserve point has a marginal strictly between the outward
    // CPMM derivative and the restoring concentrated derivative. Mapping that
    // interval to the shared shoulder is the only reserve-consistent choice;
    // directed rounding and the one-sided executable quote retain the risk
    // inequality for the selected collateral direction.
    if directed_marginal_nad < shoulder.tail_low_marginal_nad {
        return tail_risk_reserves(
            shoulder,
            directed_marginal_nad,
            below_center,
            center_price_nad,
            collateral_to_debt,
        );
    }
    if directed_marginal_nad < shoulder.inner_low_marginal_nad {
        return shoulder_risk_reserves(shoulder, below_center, center_price_nad, collateral_to_debt);
    }

    let balance_factor = solve_balance_factor_nad(
        directed_marginal_nad,
        peak_depth_nad,
        imbalance_scale_nad,
        move_away_from_center,
    )?;
    let (t, z) = shape_coordinates(balance_factor, peak_depth_nad, imbalance_scale_nad)?;
    let plus_shape = t.checked_add(z).ok_or(ErrorCode::InvariantOverflow)?;
    let minus_shape = t.checked_sub(z).ok_or(ErrorCode::InvariantOverflow)?;
    let (base_shape, quote_shape) = if below_center {
        (plus_shape, minus_shape)
    } else {
        (minus_shape, plus_shape)
    };
    let denominator = SHAPE_SCALE.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    let (base_common, quote_common) = match collateral_to_debt {
        ConcentratedSwapDirection::BaseToQuote => (
            mul_div_ceil(d, base_shape, denominator)?,
            mul_div_floor(d, quote_shape, denominator)?,
        ),
        ConcentratedSwapDirection::QuoteToBase => (
            mul_div_floor(d, base_shape, denominator)?,
            mul_div_ceil(d, quote_shape, denominator)?,
        ),
    };
    common_coordinates_to_risk_reserves(base_common, quote_common, center_price_nad, collateral_to_debt)
}

/// Reconstructs normalized reserves at a requested canonical base/quote
/// marginal price and conservative balanced-equivalent `Q`.
pub(crate) fn concentrated_risk_reserves_at_price_q(
    target_price_nad: u128,
    balanced_equivalent_q_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<ConcentratedRiskReserves> {
    validate_inputs(
        target_price_nad,
        balanced_equivalent_q_nad,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
    )?;
    if peak_depth_nad == 0 {
        cpmm_reserves_at_price_q(target_price_nad, balanced_equivalent_q_nad, collateral_to_debt)
    } else {
        concentrated_reserves_at_price_q(
            target_price_nad,
            balanced_equivalent_q_nad,
            collateral_to_debt,
            center_price_nad,
            peak_depth_nad,
            imbalance_scale_nad,
        )
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/math/concentrated_risk.rs");
}
