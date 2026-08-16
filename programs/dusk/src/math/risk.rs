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

pub(crate) fn exponential_price_decay(start_price_nad: u64, elapsed_ms: u64, half_life_ms: u64) -> Result<u64> {
    if half_life_ms == 0 || start_price_nad == 0 {
        return Ok(0); // If half-life is 0, it decays instantly.
    }
    let x = (elapsed_ms as u128)
        .saturating_mul(NATURAL_LOG_OF_TWO_NAD as u128)
        .checked_div(half_life_ms as u128)
        .unwrap_or(u128::MAX)
        .min(i64::MAX as u128) as i64;
    let alpha = taylor_exp(-x, NAD, TAYLOR_TERMS) as u128;
    let result = (start_price_nad as u128)
        .checked_mul(alpha)
        .and_then(|value| value.checked_div(NAD as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(result).map_err(|_| ErrorCode::MarketMathOverflow.into())
}
