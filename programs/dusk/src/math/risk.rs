use anchor_lang::prelude::*;

use crate::{
    constants::{
        BPS_DENOMINATOR, LTV_BUFFER_BPS, MAX_COLLATERAL_FACTOR_BPS, MS_PER_DAY, NAD, NATURAL_LOG_OF_TWO_NAD,
        TAYLOR_TERMS,
    },
    errors::ErrorCode,
    shared::math::{slots_to_ms, taylor_exp},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicCollateralTerms {
    pub max_debt_nad: u128,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
}

/// Maximum borrowable debt using V1's impact-aware dynamic collateral factor.
/// All amounts are normalized to NAD before entering this function.
pub(crate) fn pessimistic_max_debt_nad(
    collateral_amount_nad: u128,
    collateral_virtual_reserve_nad: u128,
    debt_virtual_reserve_nad: u128,
    existing_total_debt_nad: u128,
) -> Result<DynamicCollateralTerms> {
    if collateral_amount_nad == 0 || collateral_virtual_reserve_nad == 0 || debt_virtual_reserve_nad == 0 {
        return Ok(DynamicCollateralTerms::default());
    }
    if existing_total_debt_nad >= debt_virtual_reserve_nad {
        return Ok(DynamicCollateralTerms::default());
    }

    // V_impact: impact-aware collateral value using virtual reserves at
    // pessimistic price. This matches the valuation used in liquidation,
    // ensuring the borrow limit never exceeds the liquidation threshold.
    let collateral_value_with_impact = calculate_normalized_amount_out(
        collateral_virtual_reserve_nad,
        debt_virtual_reserve_nad,
        collateral_amount_nad,
    )?;
    if collateral_value_with_impact == 0 {
        return Ok(DynamicCollateralTerms::default());
    }

    // 0. Calculate utilized collateral with price impact using virtual
    // reserves at pessimistic price.
    let utilized_collateral = calculate_normalized_amount_in(
        collateral_virtual_reserve_nad,
        debt_virtual_reserve_nad,
        existing_total_debt_nad,
    )?;

    // 1. Calculate max allowed total debt using virtual reserves at
    // pessimistic price.
    let total_collateral_amount = utilized_collateral
        .checked_add(collateral_amount_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let max_allowed_total_debt = calculate_normalized_amount_out(
        collateral_virtual_reserve_nad,
        debt_virtual_reserve_nad,
        total_collateral_amount,
    )?;

    // 2. Calculate user max debt.
    let user_max_debt = max_allowed_total_debt.saturating_sub(existing_total_debt_nad);

    // 3. Calculate base CF = user max debt * BPS_DENOMINATOR / V_impact.
    // CF is relative to impact value so it captures only debt crowding.
    let base_cf_bps = user_max_debt
        .saturating_mul(BPS_DENOMINATOR as u128)
        .checked_div(collateral_value_with_impact)
        .unwrap_or(0);

    // Apply the V1 85% maximum cap on dynamic CF. No divergence cap is
    // required because the virtual reserves already use pessimistic price.
    let liquidation_cf_bps = base_cf_bps.min(MAX_COLLATERAL_FACTOR_BPS as u128) as u16;

    // Max allowed CF = liquidation CF * 95%. This creates the explicit V1
    // buffer between the borrow limit and liquidation threshold.
    let max_cf_bps = ((liquidation_cf_bps as u32).saturating_mul((BPS_DENOMINATOR - LTV_BUFFER_BPS) as u32)
        / BPS_DENOMINATOR as u32) as u16;

    // Final borrow limit = V_impact * max CF. Both underwriting and
    // liquidation use the same impact-aware collateral value.
    let max_debt_nad = collateral_value_with_impact
        .saturating_mul(max_cf_bps as u128)
        .checked_div(BPS_DENOMINATOR as u128)
        .unwrap_or(0);

    Ok(DynamicCollateralTerms {
        max_debt_nad,
        max_cf_bps,
        liquidation_cf_bps,
    })
}

use super::gamm::{calculate_normalized_amount_in, calculate_normalized_amount_out};

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

pub(crate) fn decayed_daily_bucket(bucket: u64, last_slot: u64, current_slot: u64) -> Result<u64> {
    if bucket == 0 {
        return Ok(0);
    }
    let Some(elapsed_ms) = slots_to_ms(last_slot, current_slot) else {
        return Ok(bucket);
    };
    if elapsed_ms >= MS_PER_DAY {
        return Ok(0);
    }
    let remaining_ms = (MS_PER_DAY - elapsed_ms) as u128;
    let decayed = (bucket as u128)
        .checked_mul(remaining_ms)
        .and_then(|value| value.checked_div(MS_PER_DAY as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(decayed).map_err(|_| ErrorCode::MarketMathOverflow.into())
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

#[cfg(test)]
mod tests {
    include!("../tests/math/risk.rs");
}
