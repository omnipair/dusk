use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, NAD, NATURAL_LOG_OF_TWO_NAD, TAYLOR_TERMS},
    errors::ErrorCode,
    math::{slots_to_ms, taylor_exp},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicCollateralTerms {
    pub max_debt_nad: u128,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
}

pub(crate) fn health_bps(utilized_collateral_value_nad: u128, effective_debt_nad: u128) -> Result<u64> {
    if effective_debt_nad == 0 {
        return Ok(u64::MAX);
    }
    let health = utilized_collateral_value_nad
        .checked_mul(BPS_DENOMINATOR as u128)
        .and_then(|value| value.checked_div(effective_debt_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(health).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

pub(crate) fn ema_u64(last_ema: u64, input: u64, last_slot: u64, current_slot: u64, half_life_ms: u64) -> u64 {
    if last_ema == 0 || input == 0 {
        return input;
    }
    u64::try_from(ema_u128(
        last_ema as u128,
        input as u128,
        last_slot,
        current_slot,
        half_life_ms,
    ))
    .unwrap_or(u64::MAX)
}

pub(crate) fn directional_ema_u64(
    last_ema: u64,
    input: u64,
    last_slot: u64,
    current_slot: u64,
    half_life_ms: u64,
) -> u64 {
    if last_ema == 0 || input == 0 {
        return input;
    }
    input.min(ema_u64(last_ema, input, last_slot, current_slot, half_life_ms))
}

pub(crate) fn ema_u128(last_ema: u128, input: u128, last_slot: u64, current_slot: u64, half_life_ms: u64) -> u128 {
    if last_ema == 0 || input == 0 {
        return input;
    }
    let Some(dt) = slots_to_ms(last_slot, current_slot) else {
        return last_ema;
    };
    if dt == 0 || half_life_ms == 0 {
        return last_ema;
    }
    let x = (dt as u128)
        .saturating_mul(NATURAL_LOG_OF_TWO_NAD as u128)
        .checked_div(half_life_ms as u128)
        .unwrap_or(u128::MAX)
        .min(i64::MAX as u128) as i64;
    let alpha = taylor_exp(-x, NAD, TAYLOR_TERMS) as u128;
    input
        .saturating_mul((NAD as u128).saturating_sub(alpha))
        .saturating_add(last_ema.saturating_mul(alpha))
        .checked_div(NAD as u128)
        .unwrap_or(last_ema)
}

/// EMA variant for signals where zero is a real observation rather than an
/// uninitialized sentinel. Initialization is bound to `last_slot`, allowing a
/// funding-rate signal to decay toward zero over its configured half-life.
pub(crate) fn ema_u128_including_zero(
    last_ema: u128,
    input: u128,
    last_slot: u64,
    current_slot: u64,
    half_life_ms: u64,
) -> u128 {
    if last_slot == 0 {
        return input;
    }
    let Some(dt) = slots_to_ms(last_slot, current_slot) else {
        return last_ema;
    };
    if dt == 0 || half_life_ms == 0 {
        return last_ema;
    }
    let x = (dt as u128)
        .saturating_mul(NATURAL_LOG_OF_TWO_NAD as u128)
        .checked_div(half_life_ms as u128)
        .unwrap_or(u128::MAX)
        .min(i64::MAX as u128) as i64;
    let alpha = taylor_exp(-x, NAD, TAYLOR_TERMS) as u128;
    input
        .saturating_mul((NAD as u128).saturating_sub(alpha))
        .saturating_add(last_ema.saturating_mul(alpha))
        .checked_div(NAD as u128)
        .unwrap_or(last_ema)
}

/// Utilization of a side, in bps, as `borrowed / (borrowed + idle_cash)`.
/// Returns 0 when nothing is supplied, clamped to `BPS_DENOMINATOR`.
pub fn utilization_bps(borrowed: u128, idle_cash: u128) -> Result<u64> {
    let supplied = borrowed.checked_add(idle_cash).ok_or(ErrorCode::MarketMathOverflow)?;
    if supplied == 0 {
        return Ok(0);
    }
    let util = borrowed
        .checked_mul(BPS_DENOMINATOR as u128)
        .and_then(|value| value.checked_div(supplied))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(u64::try_from(util.min(BPS_DENOMINATOR as u128)).unwrap_or(BPS_DENOMINATOR as u64))
}

/// Split a debt repayment into the principal returned to the reserve and the
/// interest owed to suppliers, proportional to the outstanding debt.
///
/// `total_debt = shares * index >= principal`, `repaid <= total_debt`. The
/// reusable primitive for non-compounding interest routing: the interest
/// portion is what a repay/close/liquidate path should move into the interest
/// vault instead of leaving in the reserve. Principal is rounded **down** (so
/// interest rounds up), ensuring the interest vault is never under-funded.
pub fn realized_interest_split(repaid: u64, total_debt: u128, principal: u128) -> Result<(u64, u64)> {
    require!(total_debt >= principal, ErrorCode::MarketMathOverflow);
    let repaid_u = repaid as u128;
    require!(repaid_u <= total_debt, ErrorCode::InsufficientDebt);
    if repaid_u == 0 {
        return Ok((0, 0));
    }
    let principal_paid = if repaid_u == total_debt {
        principal
    } else {
        principal
            .checked_mul(repaid_u)
            .and_then(|value| value.checked_div(total_debt))
            .ok_or(ErrorCode::MarketMathOverflow)?
    };
    let interest_paid = repaid_u
        .checked_sub(principal_paid)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let principal_paid = u64::try_from(principal_paid).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let interest_paid = u64::try_from(interest_paid).map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((principal_paid, interest_paid))
}

#[cfg(test)]
mod tests {
    include!("../tests/math/interest.rs");
}
