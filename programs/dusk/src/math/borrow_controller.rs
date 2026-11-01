use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, NAD},
    errors::ErrorCode,
    math::mul_div_u128,
    state::RiskRateConfig,
};

// Keep the fractional APR between calls. At slow half-lives, rounding every
// endpoint directly to NAD would let frequent accrual suppress rate movement.
pub const RATE_SCALE: u128 = 1_000_000_000_000_000_000;
const RATE_TO_NAD: u128 = RATE_SCALE / NAD as u128;
const LN_TWO: u128 = 693_147_180_559_945_309;
/// `maximum_apr_nad == 0` means no governance-selected economic ceiling. The
/// endpoint still needs a representable bound so a valid, stale market cannot
/// overflow before borrowers can repay or governance can install a new model.
/// `u64::MAX` is also the largest explicit ceiling the public config can name.
const UNBOUNDED_NUMERIC_APR_NAD: u128 = u64::MAX as u128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateAccrual {
    pub end_apr_e18: u128,
    pub integrated_apr_nad_ms: u128,
}

/// One funded junior budget divided by consolidated debt, then discounted by
/// this asset's idle fraction. Both value inputs must use the same numeraire.
/// No debt returns None: the controller holds its current rate.
pub fn coverage_signal_nad(junior_value: u128, covered_debt_value: u128, utilization_bps: u64) -> Result<Option<u128>> {
    require!(utilization_bps <= BPS_DENOMINATOR as u64, ErrorCode::InvalidArgument);
    if covered_debt_value == 0 {
        return Ok(None);
    }
    // Multiplying the two fixed scales first avoids an intermediate J/D
    // overflow even when the asset is fully utilized (and its signal is zero).
    let idle_scale = (BPS_DENOMINATOR as u64 - utilization_bps) as u128 * (NAD as u128 / BPS_DENOMINATOR as u128);
    Ok(Some(mul_div_u128(junior_value, idle_scale, covered_debt_value)?))
}

/// V1's exponential up/hold/down mechanism with reversed coverage direction.
/// Integrates the changing APR over the complete elapsed interval, including
/// any time spent at a configured floor/ceiling. No instantaneous utilization
/// multiplier or truncated per-millisecond exponential coefficient is used.
pub fn accrue_risk_rate(
    config: &RiskRateConfig,
    previous_apr_e18: u128,
    signal_nad: Option<u128>,
    elapsed_ms: u64,
) -> Result<RateAccrual> {
    config.validate()?;
    if elapsed_ms == 0 {
        return Ok(RateAccrual {
            end_apr_e18: previous_apr_e18,
            integrated_apr_nad_ms: 0,
        });
    }
    let minimum = config.minimum_apr_nad as u128 * RATE_TO_NAD;
    let maximum_nad = if config.maximum_apr_nad == 0 {
        UNBOUNDED_NUMERIC_APR_NAD
    } else {
        config.maximum_apr_nad as u128
    };
    let maximum = maximum_nad * RATE_TO_NAD;
    let start = previous_apr_e18.max(minimum);
    let start = if maximum == 0 { start } else { start.min(maximum) };
    let direction = match signal_nad {
        Some(value) if value < config.lower_signal_nad as u128 => 1,
        Some(value) if value > config.upper_signal_nad as u128 => -1,
        _ => 0,
    };
    if direction == 0 || start == 0 {
        return Ok(RateAccrual {
            end_apr_e18: start,
            integrated_apr_nad_ms: mul_div_u128(start, elapsed_ms as u128, RATE_TO_NAD)?,
        });
    }
    if direction > 0 && config.maximum_apr_nad == 0 {
        // Preserve the cheap ordinary path for the default uncapped model. Only
        // pay for a logarithmic crossing calculation when the elapsed interval
        // actually reaches the numeric endpoint (or direct evolution overflows).
        if let Ok(end) = evolve(start, elapsed_ms, config.half_life_ms, true) {
            if end <= maximum {
                let change = end - start;
                return Ok(RateAccrual {
                    end_apr_e18: end,
                    integrated_apr_nad_ms: mul_div_u128(change, config.half_life_ms as u128 * RATE_TO_NAD, LN_TWO)?,
                });
            }
        }
    }
    let bound = if direction > 0 { maximum } else { minimum };
    let crossing_ms = if bound == 0 {
        // A zero minimum is asymptotic: there is no finite half-life crossing.
        u64::MAX
    } else {
        let (high, low) = if direction > 0 { (bound, start) } else { (start, bound) };
        half_life_crossing_ms(high, low, config.half_life_ms)?
    };
    let exponential_ms = elapsed_ms.min(crossing_ms);
    let end = evolve(start, exponential_ms, config.half_life_ms, direction > 0)?;
    let change = start.abs_diff(end);
    let area = mul_div_u128(change, config.half_life_ms as u128 * RATE_TO_NAD, LN_TWO)?;
    let at_bound = elapsed_ms >= crossing_ms;
    let tail = if at_bound {
        mul_div_u128(bound, (elapsed_ms - exponential_ms) as u128, RATE_TO_NAD)?
    } else {
        0
    };
    Ok(RateAccrual {
        end_apr_e18: if at_bound { bound } else { end },
        integrated_apr_nad_ms: area.checked_add(tail).ok_or(ErrorCode::MarketMathOverflow)?,
    })
}

// Compute 2^(fractional half-life), with an exponent in [0, ln(2)). Eighteen
// terms have less than one e18 unit of analytic truncation error on this range.
fn fractional_factor(elapsed_ms: u64, half_life_ms: u64) -> Result<u128> {
    let x = LN_TWO * (elapsed_ms % half_life_ms) as u128 / half_life_ms as u128;
    let mut term = RATE_SCALE;
    let mut sum = RATE_SCALE;
    for n in 1..=18 {
        term = mul_div_u128(term, x, RATE_SCALE * n)?;
        sum += term;
        if term == 0 {
            break;
        }
    }
    Ok(sum)
}

fn evolve(start: u128, elapsed_ms: u64, half_life_ms: u64, rising: bool) -> Result<u128> {
    let whole = elapsed_ms / half_life_ms;
    let factor = fractional_factor(elapsed_ms, half_life_ms)?;
    if rising {
        require!(whole < 128, ErrorCode::MarketMathOverflow);
        let doubled = start
            .checked_mul(1_u128 << whole)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        mul_div_u128(doubled, factor, RATE_SCALE)
    } else if whole >= 128 {
        Ok(0)
    } else {
        mul_div_u128(start >> whole, RATE_SCALE, factor)
    }
}

// log2(high/low), with 32 fractional bits. Each iteration squares a normalized
// [1, 2) value, so the product fits u128. Crossing is rounded down by less than
// one millisecond at the maximum accepted half-life, apart from fixed-point
// rounding. A bound may therefore engage at most one millisecond early.
fn half_life_crossing_ms(high: u128, low: u128, half_life_ms: u64) -> Result<u64> {
    require!(low > 0 && high >= low, ErrorCode::InvalidArgument);
    let mut whole = high.ilog2() - low.ilog2();
    if (high >> whole) < low {
        whole -= 1;
    }
    let mut ratio = mul_div_u128(high, RATE_SCALE, low << whole)?;
    let mut fraction = 0_u128;
    for bit in (0..32).rev() {
        ratio = ratio * ratio / RATE_SCALE;
        if ratio >= 2 * RATE_SCALE {
            ratio /= 2;
            fraction |= 1_u128 << bit;
        }
    }
    let time = ((((whole as u128) << 32) + fraction) * half_life_ms as u128) >> 32;
    u64::try_from(time).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

/// Admission only: this must not gate repayment, cash-backed LP withdrawal or
/// cashless tranche conversion. There is deliberately no junior-capital check.
/// Values are atom amounts of one borrowed asset, including its hLP debt.
pub fn check_new_borrow_liquidity(
    config: &RiskRateConfig,
    debt_before: u128,
    cash_before: u64,
    debt_increase: u128,
    cash_outflow: u64,
) -> Result<()> {
    config.validate()?;
    require!(cash_outflow <= cash_before, ErrorCode::InsufficientLiquidity);
    let debt_after = debt_before
        .checked_add(debt_increase)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let total_after = debt_after
        .checked_add((cash_before - cash_outflow) as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if total_after == 0 {
        return Ok(());
    }
    require!(
        crate::math::ratio_lte_full_width(
            debt_after,
            total_after,
            config.borrow_utilization_limit_bps as u128,
            BPS_DENOMINATOR as u128
        )?,
        ErrorCode::InsufficientLiquidity
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../tests/math/borrow_controller.rs");
}
