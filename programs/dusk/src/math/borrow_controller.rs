use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, NAD},
    errors::ErrorCode,
    math::mul_div_u128,
    state::RiskRateConfig,
    transitions::tranches::MINIMUM_BORROW_APR_NAD,
};

// Keep the fractional APR between calls. At slow half-lives, rounding every
// endpoint directly to NAD would let frequent accrual suppress rate movement.
pub const RATE_SCALE: u128 = 1_000_000_000_000_000_000;
const RATE_TO_NAD: u128 = RATE_SCALE / NAD as u128;
const LN_TWO: u128 = 693_147_180_559_945_309;
/// A representational endpoint keeps stale markets accruable without an
/// operator-selected economic ceiling.
const UNBOUNDED_NUMERIC_APR_NAD: u128 = u64::MAX as u128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateAccrual {
    pub end_apr_e18: u128,
    pub integrated_apr_nad_ms: u128,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RiskRateInput {
    pub signal_nad: Option<u128>,
    pub utilization_bps: u64,
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

/// V1's exponential up/hold/down mechanism driven by independent inclusive
/// risk-signal and utilization bands. Undercoverage or excess utilization
/// raises the APR. The APR falls only when coverage is above its band and
/// utilization is below its band; all other combinations hold. The changing
/// APR is integrated over the complete elapsed interval, including time at the
/// fixed floor. There is no instantaneous utilization multiplier.
pub fn accrue_risk_rate(
    config: &RiskRateConfig,
    previous_apr_e18: u128,
    input: RiskRateInput,
    elapsed_ms: u64,
) -> Result<RateAccrual> {
    config.validate()?;
    require!(
        input.utilization_bps <= BPS_DENOMINATOR as u64,
        ErrorCode::InvalidArgument
    );
    if elapsed_ms == 0 {
        return Ok(RateAccrual {
            end_apr_e18: previous_apr_e18,
            integrated_apr_nad_ms: 0,
        });
    }
    let minimum = MINIMUM_BORROW_APR_NAD as u128 * RATE_TO_NAD;
    let maximum = UNBOUNDED_NUMERIC_APR_NAD * RATE_TO_NAD;
    let start = previous_apr_e18.max(minimum);
    let start = if maximum == 0 { start } else { start.min(maximum) };
    let direction = match input.signal_nad {
        Some(value)
            if value < config.lower_signal_nad as u128
                || input.utilization_bps > config.upper_utilization_bps as u64 =>
        {
            1
        }
        Some(value)
            if value > config.upper_signal_nad as u128
                && input.utilization_bps < config.lower_utilization_bps as u64 =>
        {
            -1
        }
        _ => 0,
    };
    if direction == 0 || start == 0 {
        return Ok(RateAccrual {
            end_apr_e18: start,
            integrated_apr_nad_ms: mul_div_u128(start, elapsed_ms as u128, RATE_TO_NAD)?,
        });
    }
    if direction > 0 {
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

#[cfg(test)]
mod tests {
    include!("../tests/math/borrow_controller.rs");
}
