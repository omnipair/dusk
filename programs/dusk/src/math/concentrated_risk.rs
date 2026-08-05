//! Conservative reserve reconstruction for Dusk Concentrated AMM risk views.
//!
//! The invariant is homogeneous: scaling both reserves scales invariant `D`
//! and balanced-equivalent `Q`, while leaving marginal price unchanged. Risk
//! snapshots therefore solve a dimensionless shape at the requested price and
//! then scale that shape to a deliberately reduced `Q` budget.
//!
//! Inside the inner region, balance factor `b = 4*x*y/D^2` and
//! `delta = 1-b` give the normalized reserve sum directly:
//! ```text
//! t = (x+y)/D
//! t = 1 + delta*(fade_scale+delta)^2
//!         / (2*peak_depth*b*fade_scale^2)
//! ```
//! The inner coordinates follow from their sum and product. The protocol-fixed
//! convergence transition is searched in the imbalance coordinate `v`; beyond
//! it, reconstruction uses the exact CPMM continuation. This module never
//! nests the public quote or invariant solvers inside the risk-price search.

use anchor_lang::prelude::*;

use crate::{constants::NAD, errors::ErrorCode};

use super::{
    concentrated::{validate_parameters, ConcentratedC1Geometry, Q48_ONE},
    concentrated_marginal_price_from_common, isqrt, ConcentratedCommonNumeraire, ConcentratedSwapDirection,
    CONCENTRATED_MAX_FADE_SCALE_NAD, CONCENTRATED_MAX_PEAK_DEPTH_NAD, MAX_COMMON_RESERVE,
};

// Production-wide arithmetic is deliberately isolated to this risk-view
// reconstruction module. It is not reachable from an ordinary swap quote.
//
// `balanced_equivalent_q_nad` is bounded by one raw `u64` token balance
// normalized from zero decimals (`u64::MAX * NAD < 2^94`), while prices are
// bounded by `u64::MAX`. The largest exact radicand is therefore
// `4*q^2*price < 2^254`: too wide for `u128`, but strictly within `U256`.
// Tail products are below `2^158`, while every 1e18-precision shape
// intermediate is below `2^176`. No production calculation in this module
// needs U512. The risk shape deliberately retains its existing 1e18 scale:
// reducing it to Q48 can move a `u64`-scale reconstructed reserve by more than
// one atom, while this non-swap path gains nothing from the lower precision.
#[allow(clippy::assign_op_pattern, clippy::manual_div_ceil)]
mod wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U256(4);
    }
}

use wide::U256;

const SHAPE_SCALE: u128 = 1_000_000_000_000_000_000;
const PPM_DENOMINATOR: u128 = 1_000_000;
const MAX_BALANCED_EQUIVALENT_Q_NAD: u128 = (u64::MAX as u128) * (NAD as u128);

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

fn u256_to_u128(value: U256) -> Result<u128> {
    require!(value <= U256::from(u128::MAX), ErrorCode::InvariantOverflow);
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

fn sqrt_floor_u256_to_u128(value: U256) -> Result<u128> {
    if value.is_zero() {
        return Ok(0);
    }
    let max_root = U256::from(u128::MAX);
    require!(
        value <= max_root.checked_mul(max_root).ok_or(ErrorCode::InvariantOverflow)?,
        ErrorCode::InvariantOverflow
    );
    let mut root = U256::one() << value.bits().div_ceil(2);
    for _ in 0..CONCENTRATED_RISK_SQRT_MAX_ITERS {
        #[cfg(test)]
        SQRT_ITERATIONS.with(|count| count.set(count.get() + 1));
        let next = root.checked_add(value / root).ok_or(ErrorCode::InvariantOverflow)? >> 1;
        if next >= root {
            return u256_to_u128(root);
        }
        root = next;
    }
    err!(ErrorCode::InvariantOverflow)
}

fn sqrt_ceil_u256_to_u128(value: U256) -> Result<u128> {
    let floor = sqrt_floor_u256_to_u128(value)?;
    let square = U256::from(floor)
        .checked_mul(U256::from(floor))
        .ok_or(ErrorCode::InvariantOverflow)?;
    if square == value {
        Ok(floor)
    } else {
        floor.checked_add(1).ok_or_else(|| ErrorCode::InvariantOverflow.into())
    }
}

fn conservative_q(balanced_equivalent_q_nad: u128) -> Result<u128> {
    let kept_ppm = PPM_DENOMINATOR
        .checked_sub(CONCENTRATED_RISK_Q_SAFETY_PPM)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let q = mul_div_floor(balanced_equivalent_q_nad, kept_ppm, PPM_DENOMINATOR)?;
    require!(q > 0, ErrorCode::InsufficientLiquidity);
    Ok(q)
}

fn common_coordinates_to_risk_reserves(
    base_common: u128,
    quote_common: u128,
    center_price_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
) -> Result<ConcentratedRiskReserves> {
    require!(
        base_common > 0 && quote_common > 0 && base_common <= MAX_COMMON_RESERVE && quote_common <= MAX_COMMON_RESERVE,
        ErrorCode::InsufficientLiquidity
    );
    let numeraire = ConcentratedCommonNumeraire::for_center(center_price_nad)?;
    let (base_reserve_nad, quote_reserve_nad) = match (numeraire, collateral_to_debt) {
        (ConcentratedCommonNumeraire::Quote, ConcentratedSwapDirection::BaseToQuote) => (
            numeraire
                .base_scale(center_price_nad)?
                .common_to_raw_ceil(base_common)?,
            quote_common,
        ),
        (ConcentratedCommonNumeraire::Quote, ConcentratedSwapDirection::QuoteToBase) => (
            numeraire
                .base_scale(center_price_nad)?
                .common_to_raw_floor(base_common)?,
            quote_common,
        ),
        (ConcentratedCommonNumeraire::Base, ConcentratedSwapDirection::BaseToQuote) => (
            base_common,
            numeraire
                .quote_scale(center_price_nad)?
                .common_to_raw_floor(quote_common)?,
        ),
        (ConcentratedCommonNumeraire::Base, ConcentratedSwapDirection::QuoteToBase) => (
            base_common,
            numeraire
                .quote_scale(center_price_nad)?
                .common_to_raw_ceil(quote_common)?,
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

/// Returns `(t, z)` at SHAPE_SCALE precision, where reserves in common
/// coordinates are proportional to `(t+z, t-z)`.
fn shape_coordinates(balance_factor_nad: u128, peak_depth_nad: u128, fade_scale_nad: u128) -> Result<(u128, u128)> {
    require!(
        balance_factor_nad > 0 && balance_factor_nad <= NAD as u128,
        ErrorCode::InvalidArgument
    );
    if balance_factor_nad == NAD as u128 {
        return Ok((SHAPE_SCALE, 0));
    }
    let delta = (NAD as u128) - balance_factor_nad;
    let scale_plus_delta = fade_scale_nad.checked_add(delta).ok_or(ErrorCode::InvariantOverflow)?;
    require!(
        peak_depth_nad > 0
            && fade_scale_nad > 0
            && delta <= fade_scale_nad
            && peak_depth_nad <= CONCENTRATED_MAX_PEAK_DEPTH_NAD
            && fade_scale_nad <= CONCENTRATED_MAX_FADE_SCALE_NAD,
        ErrorCode::InvalidArgument
    );
    let numerator = U256::from(delta)
        .checked_mul(U256::from(scale_plus_delta))
        .and_then(|value| value.checked_mul(U256::from(scale_plus_delta)))
        .and_then(|value| value.checked_mul(U256::from(NAD)))
        .and_then(|value| value.checked_mul(U256::from(SHAPE_SCALE)))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let denominator = U256::from(2_u8)
        .checked_mul(U256::from(peak_depth_nad))
        .and_then(|value| value.checked_mul(U256::from(balance_factor_nad)))
        .and_then(|value| value.checked_mul(U256::from(fade_scale_nad)))
        .and_then(|value| value.checked_mul(U256::from(fade_scale_nad)))
        .ok_or(ErrorCode::InvariantOverflow)?;
    require!(!denominator.is_zero(), ErrorCode::DenominatorOverflow);
    let offset = u256_to_u128(numerator / denominator)?;
    let t = SHAPE_SCALE.checked_add(offset).ok_or(ErrorCode::InvariantOverflow)?;
    let t_squared = U256::from(t)
        .checked_mul(U256::from(t))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let product_term = U256::from(balance_factor_nad)
        .checked_mul(U256::from(SHAPE_SCALE))
        .and_then(|value| value.checked_mul(U256::from(SHAPE_SCALE)))
        .ok_or(ErrorCode::InvariantOverflow)?
        / U256::from(NAD);
    let z = sqrt_floor_u256_to_u128(
        t_squared
            .checked_sub(product_term)
            .ok_or(ErrorCode::InvariantOverflow)?,
    )?;
    Ok((t, z))
}

/// Returns the low/high reserve factors in Q48 relative to `D/2` for a
/// transition coordinate `(q, v)`.
fn transition_shape_factors_q48(q_q48: u128, v_q48: u128) -> Result<(u128, u128)> {
    require!(q_q48 > 0 && q_q48 <= Q48_ONE, ErrorCode::InvalidArgument);
    let sqrt_q_q48 = isqrt(q_q48.checked_mul(Q48_ONE).ok_or(ErrorCode::InvariantOverflow)?);
    let cosh_q48 = isqrt(
        Q48_ONE
            .checked_mul(Q48_ONE)
            .and_then(|one_squared| {
                v_q48
                    .checked_mul(v_q48)
                    .and_then(|v_squared| one_squared.checked_add(v_squared))
            })
            .ok_or(ErrorCode::InvariantOverflow)?,
    );
    let low_factor = mul_div_floor(
        sqrt_q_q48,
        cosh_q48.checked_sub(v_q48).ok_or(ErrorCode::InvariantOverflow)?,
        Q48_ONE,
    )?;
    let high_factor = mul_div_floor(
        sqrt_q_q48,
        cosh_q48.checked_add(v_q48).ok_or(ErrorCode::InvariantOverflow)?,
        Q48_ONE,
    )?;
    require!(
        low_factor > 0 && high_factor >= low_factor,
        ErrorCode::InvariantOverflow
    );
    Ok((low_factor, high_factor))
}

/// Reconstructs normalized reserves at a requested canonical base/quote
/// marginal price and conservative balanced-equivalent `Q`.
pub(crate) fn concentrated_risk_reserves_at_price_q(
    target_price_nad: u128,
    balanced_equivalent_q_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<ConcentratedRiskReserves> {
    require!(
        target_price_nad > 0
            && target_price_nad <= u64::MAX as u128
            && balanced_equivalent_q_nad > 0
            && balanced_equivalent_q_nad <= MAX_BALANCED_EQUIVALENT_Q_NAD
            && center_price_nad > 0
            && center_price_nad <= u64::MAX as u128,
        ErrorCode::InvalidArgument
    );
    validate_parameters(center_price_nad, peak_depth_nad, fade_scale_nad)?;
    if peak_depth_nad == 0 {
        let q = conservative_q(balanced_equivalent_q_nad)?;
        let q_squared = U256::from(q)
            .checked_mul(U256::from(q))
            .ok_or(ErrorCode::InvariantOverflow)?;
        let base_squared = q_squared
            .checked_mul(U256::from(NAD))
            .ok_or(ErrorCode::InvariantOverflow)?
            / U256::from(target_price_nad);
        let quote_squared = q_squared
            .checked_mul(U256::from(target_price_nad))
            .ok_or(ErrorCode::InvariantOverflow)?
            / U256::from(NAD);
        let (base_reserve_nad, quote_reserve_nad) = match collateral_to_debt {
            ConcentratedSwapDirection::BaseToQuote => (
                sqrt_ceil_u256_to_u128(base_squared)?,
                sqrt_floor_u256_to_u128(quote_squared)?,
            ),
            ConcentratedSwapDirection::QuoteToBase => (
                sqrt_floor_u256_to_u128(base_squared)?,
                sqrt_ceil_u256_to_u128(quote_squared)?,
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
    } else {
        let q = conservative_q(balanced_equivalent_q_nad)?;
        let (d_ratio_numerator, d_ratio_denominator) = if center_price_nad >= NAD as u128 {
            (center_price_nad, NAD as u128)
        } else {
            (NAD as u128, center_price_nad)
        };
        let radicand = U256::from(q)
            .checked_mul(U256::from(q))
            .and_then(|value| value.checked_mul(U256::from(d_ratio_numerator)))
            .and_then(|value| value.checked_mul(U256::from(4_u8)))
            .ok_or(ErrorCode::InvariantOverflow)?
            / U256::from(d_ratio_denominator);
        let d = sqrt_floor_u256_to_u128(radicand)?;
        require!(d > 0, ErrorCode::InsufficientLiquidity);
        let geometry = ConcentratedC1Geometry::derive(peak_depth_nad, fade_scale_nad)?;
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
        require!(
            directed_marginal_nad > 0 && directed_marginal_nad <= NAD as u128,
            ErrorCode::InvalidArgument
        );

        let shape_d = SHAPE_SCALE.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
        let transition_marginal = |v_q48: u128, q_q48: u128| -> Result<u128> {
            let (low_factor, high_factor) = transition_shape_factors_q48(q_q48, v_q48)?;
            let low_common = mul_div_floor(SHAPE_SCALE, low_factor, Q48_ONE)?;
            let high_common = mul_div_floor(SHAPE_SCALE, high_factor, Q48_ONE)?;
            require!(
                low_common > 0 && high_common > low_common,
                ErrorCode::InsufficientLiquidity
            );
            concentrated_marginal_price_from_common(
                high_common,
                low_common,
                shape_d,
                NAD as u128,
                peak_depth_nad,
                fade_scale_nad,
            )
        };
        let start_marginal_nad = transition_marginal(geometry.v_start_q48, geometry.q_start_q48)?;
        let tail_marginal_nad = transition_marginal(geometry.v_tail_q48, geometry.q_tail_q48)?;
        require!(
            tail_marginal_nad > 0 && tail_marginal_nad < start_marginal_nad,
            ErrorCode::BrokenInvariant
        );

        if directed_marginal_nad < tail_marginal_nad {
            let product = U256::from(d)
                .checked_mul(U256::from(d))
                .and_then(|value| value.checked_mul(U256::from(geometry.q_tail_q48)))
                .ok_or(ErrorCode::InvariantOverflow)?
                / U256::from(Q48_ONE.checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?);
            let low_squared = product
                .checked_mul(U256::from(directed_marginal_nad))
                .ok_or(ErrorCode::InvariantOverflow)?
                / U256::from(NAD);
            let high_squared = product
                .checked_mul(U256::from(NAD))
                .ok_or(ErrorCode::InvariantOverflow)?
                / U256::from(directed_marginal_nad);
            let low_floor = sqrt_floor_u256_to_u128(low_squared)?;
            let low_ceil = sqrt_ceil_u256_to_u128(low_squared)?;
            let high_floor = sqrt_floor_u256_to_u128(high_squared)?;
            let high_ceil = sqrt_ceil_u256_to_u128(high_squared)?;
            let base_is_low = !below_center;
            let base_rounds_up = collateral_to_debt == ConcentratedSwapDirection::BaseToQuote;
            let base_common = match (base_is_low, base_rounds_up) {
                (true, true) => low_ceil,
                (true, false) => low_floor,
                (false, true) => high_ceil,
                (false, false) => high_floor,
            };
            let quote_common = match (!base_is_low, !base_rounds_up) {
                (true, true) => low_ceil,
                (true, false) => low_floor,
                (false, true) => high_ceil,
                (false, false) => high_floor,
            };
            return common_coordinates_to_risk_reserves(
                base_common,
                quote_common,
                center_price_nad,
                collateral_to_debt,
            );
        }

        if directed_marginal_nad < start_marginal_nad {
            let mut inner_v = geometry.v_start_q48;
            let mut outer_v = geometry.v_tail_q48;
            for _ in 0..CONCENTRATED_RISK_PRICE_MAX_ITERS {
                if outer_v - inner_v <= 1 {
                    break;
                }
                let midpoint = inner_v + (outer_v - inner_v) / 2;
                #[cfg(test)]
                SHAPE_EVALUATIONS.with(|count| count.set(count.get() + 1));
                let (midpoint_q, _) = geometry.transition_q_and_slope_at_v(midpoint)?;
                if transition_marginal(midpoint, midpoint_q)? <= directed_marginal_nad {
                    outer_v = midpoint;
                } else {
                    inner_v = midpoint;
                }
            }
            let selected_v = if move_away_from_center { outer_v } else { inner_v };
            let (selected_q, _) = geometry.transition_q_and_slope_at_v(selected_v)?;
            let (low_factor, high_factor) = transition_shape_factors_q48(selected_q, selected_v)?;
            let (base_factor, quote_factor) = if below_center {
                (high_factor, low_factor)
            } else {
                (low_factor, high_factor)
            };
            let denominator = Q48_ONE.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
            let (base_common, quote_common) = match collateral_to_debt {
                ConcentratedSwapDirection::BaseToQuote => (
                    mul_div_ceil(d, base_factor, denominator)?,
                    mul_div_floor(d, quote_factor, denominator)?,
                ),
                ConcentratedSwapDirection::QuoteToBase => (
                    mul_div_floor(d, base_factor, denominator)?,
                    mul_div_ceil(d, quote_factor, denominator)?,
                ),
            };
            return common_coordinates_to_risk_reserves(
                base_common,
                quote_common,
                center_price_nad,
                collateral_to_debt,
            );
        }

        let balance_factor = if directed_marginal_nad == NAD as u128 {
            NAD as u128
        } else {
            let mut high = NAD as u128;
            let mut low = mul_div_floor(geometry.q_start_q48, NAD as u128, Q48_ONE)?;
            require!(low > 0, ErrorCode::InvalidArgument);
            for _ in 0..CONCENTRATED_RISK_PRICE_MAX_ITERS {
                if high - low <= 1 {
                    break;
                }
                let midpoint = low + (high - low) / 2;
                #[cfg(test)]
                SHAPE_EVALUATIONS.with(|count| count.set(count.get() + 1));
                let (shape_t, shape_z) = shape_coordinates(midpoint, peak_depth_nad, fade_scale_nad)?;
                let x = shape_t.checked_add(shape_z).ok_or(ErrorCode::InvariantOverflow)?;
                let y = shape_t.checked_sub(shape_z).ok_or(ErrorCode::InvariantOverflow)?;
                require!(y > 0, ErrorCode::InsufficientLiquidity);
                let marginal_nad = concentrated_marginal_price_from_common(
                    x,
                    y,
                    shape_d,
                    NAD as u128,
                    peak_depth_nad,
                    fade_scale_nad,
                )?;
                if marginal_nad <= directed_marginal_nad {
                    low = midpoint;
                } else {
                    high = midpoint;
                }
            }
            if move_away_from_center {
                low
            } else {
                high
            }
        };
        let (t, z) = shape_coordinates(balance_factor, peak_depth_nad, fade_scale_nad)?;
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
}

#[cfg(test)]
mod tests {
    include!("../tests/math/concentrated_risk.rs");
}
