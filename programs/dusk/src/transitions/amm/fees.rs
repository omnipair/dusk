use anchor_lang::prelude::*;

use crate::{
    constants::{
        BPS_DENOMINATOR, MAX_PARAMETER_FEE_BPS, NAD, NATURAL_LOG_OF_TWO_NAD, TARGET_MS_PER_SLOT, TAYLOR_TERMS,
    },
    errors::ErrorCode,
    math::{ceil_div, taylor_exp},
};

const Q64: u128 = 1_u128 << 64;

/// All fee rates and signals use NAD precision (`NAD == 100%`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicFeeConfig {
    pub base_fee_bps: u16,
    pub divergence_fee_share_cap_bps: u16,
    pub volatility_fee_share_cap_bps: u16,
    /// Near-center marginal fee coefficient for squared outward displacement.
    pub divergence_coefficient_nad: u64,
    /// Near-zero pressure coefficient for the decayed volatility accumulator.
    /// The protocol maps that pressure asymptotically below 100%; this
    /// coefficient is a sensitivity control, not a fee-rate cap.
    pub volatility_coefficient_nad: u64,
    pub volatility_half_life_ms: u64,
    pub volatility_shock_cap_nad: u64,
    pub volatility_accumulator_cap_nad: u64,
}

pub(crate) fn validate_fee_share_caps(
    base_fee_bps: u16,
    divergence_fee_share_cap_bps: u16,
    volatility_fee_share_cap_bps: u16,
) -> Result<()> {
    let hard_cap_bps = MAX_PARAMETER_FEE_BPS;
    require!(
        base_fee_bps <= hard_cap_bps
            && divergence_fee_share_cap_bps <= hard_cap_bps
            && volatility_fee_share_cap_bps <= hard_cap_bps,
        ErrorCode::InvalidSwapFeeBps
    );
    let aggregate_bps = base_fee_bps
        .checked_add(divergence_fee_share_cap_bps)
        .and_then(|value| value.checked_add(volatility_fee_share_cap_bps))
        .ok_or(ErrorCode::InvalidSwapFeeBps)?;
    require!(aggregate_bps <= hard_cap_bps, ErrorCode::InvalidSwapFeeBps);
    Ok(())
}

/// Floors one component's budget against the original gross input.
pub(crate) fn gross_fee_budget_floor(gross_input: u64, fee_share_bps: u16) -> Result<u64> {
    require!(fee_share_bps <= BPS_DENOMINATOR, ErrorCode::InvalidSwapFeeBps);
    u64::try_from(
        (gross_input as u128)
            .checked_mul(fee_share_bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::FeeMathOverflow)?,
    )
    .map_err(|_| ErrorCode::FeeMathOverflow.into())
}

pub(crate) const fn hard_total_fee_budget_floor(gross_input: u64) -> u64 {
    gross_input / 2
}

/// Converts a maximum share of component gross into a maximum marginal toll
/// over executable input: `share / (1 - share)`, rounded down.
pub(crate) fn fee_share_cap_to_marginal_rate_nad(fee_share_cap_bps: u16) -> Result<u64> {
    require!(fee_share_cap_bps <= MAX_PARAMETER_FEE_BPS, ErrorCode::InvalidSwapFeeBps);
    if fee_share_cap_bps == 0 {
        return Ok(0);
    }
    let denominator_bps = BPS_DENOMINATOR
        .checked_sub(fee_share_cap_bps)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    u64::try_from(
        (fee_share_cap_bps as u128)
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(denominator_bps as u128))
            .ok_or(ErrorCode::FeeMathOverflow)?,
    )
    .map_err(|_| ErrorCode::FeeMathOverflow.into())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicFeePreState {
    /// Frozen concentration center used for every pass of the quote.
    pub center_price_nad: u64,
    /// Accumulator as committed by the last successful swap.
    pub volatility_accumulator_nad: u64,
    pub volatility_last_update_slot: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicFeeQuote {
    pub base_rate_nad: u64,
    pub divergence_rate_nad: u64,
    pub volatility_rate_nad: u64,
    pub total_rate_nad: u64,
    pub base_fee_amount: u64,
    pub divergence_surcharge_amount: u64,
    pub volatility_surcharge_amount: u64,
    pub dynamic_surcharge_amount: u64,
    pub total_fee_amount: u64,
    /// State after applying elapsed-time decay, before the quoted move.
    pub decayed_volatility_nad: u64,
    /// Candidate state to commit only after the swap succeeds.
    pub post_success_volatility_nad: u64,
}

/// Raw-token state potential for u64 reserve coordinates:
///
/// `F(u) = 4*c*u^3 / [denominator_scale*q0*(q0+u)]`.
///
/// The integer part of `u^3/[q0(q0+u)]` is exact. Its remainder is carried at
/// Q64 precision, which bounds final endpoint error below one raw atom for the
/// configured coefficient range. Endpoint differencing therefore remains
/// telescoping and split-resistant without U256/U512 arithmetic.
fn uncapped_divergence_state_potential_raw_saturating(
    outward_coordinate_raw: u64,
    center_input_reserve_raw: u64,
    coefficient_nad: u64,
) -> Result<(u128, bool)> {
    if outward_coordinate_raw == 0 || coefficient_nad == 0 {
        return Ok((0, false));
    }
    require!(center_input_reserve_raw > 0, ErrorCode::InvalidArgument);
    let denominator_scale = (NAD as u128).checked_mul(3).ok_or(ErrorCode::MarketMathOverflow)?;
    let endpoint = center_input_reserve_raw
        .checked_add(outward_coordinate_raw)
        .ok_or(ErrorCode::ReserveOverflow)?;
    let outward = outward_coordinate_raw as u128;
    let center = center_input_reserve_raw as u128;
    let endpoint = endpoint as u128;

    let outward_squared = outward.checked_mul(outward).ok_or(ErrorCode::MarketMathOverflow)?;
    let first_quotient = outward_squared / center;
    let first_remainder = outward_squared % center;
    // Here `outward` and `endpoint` are raw u64 reserve coordinates. Split
    // before multiplying: the only remainder product is therefore 64x64 and
    // cannot overflow u128. Sending this stage through the generic 128-round
    // fallback is both unnecessary and dominant in stressed divergence quotes.
    let base_shape = match (first_quotient / endpoint).checked_mul(outward) {
        Some(value) => value,
        None => return Ok((u128::MAX, true)),
    };
    let remainder_product = (first_quotient % endpoint) * outward;
    let mut shape = match base_shape.checked_add(remainder_product / endpoint) {
        Some(value) => value,
        None => return Ok((u128::MAX, true)),
    };
    let second_remainder = remainder_product % endpoint;

    // Fraction = second_remainder/endpoint
    //          + (first_remainder/center)*(outward/endpoint).
    let first_fraction_q64 = (second_remainder << 64) / endpoint;
    let carried_fraction_q64 = ((first_remainder << 64) / center)
        .checked_mul(outward)
        .ok_or(ErrorCode::MarketMathOverflow)?
        / endpoint;
    let mut fraction_q64 = first_fraction_q64
        .checked_add(carried_fraction_q64)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if fraction_q64 >= Q64 {
        shape = match shape.checked_add(1) {
            Some(value) => value,
            None => return Ok((u128::MAX, true)),
        };
        fraction_q64 -= Q64;
    }

    let coefficient_times_four = (coefficient_nad as u128)
        .checked_mul(4)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Split before multiplying. `shape / denominator_scale` contributes the
    // integer part directly, while the remainder product is bounded by
    // `denominator_scale * (4 * u64::MAX)` and therefore fits u128. If the
    // integer product overflows, the final quotient cannot fit u128 either.
    let Some(integer_product) = (shape / denominator_scale).checked_mul(coefficient_times_four) else {
        return Ok((u128::MAX, true));
    };
    let remainder_product = (shape % denominator_scale)
        .checked_mul(coefficient_times_four)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let Some(whole) = integer_product.checked_add(remainder_product / denominator_scale) else {
        return Ok((u128::MAX, true));
    };
    let remainder = remainder_product % denominator_scale;
    let Some(fraction_product) = fraction_q64.checked_mul(coefficient_times_four) else {
        return Ok((u128::MAX, true));
    };
    let fractional_whole = fraction_product >> 64;
    let correction = remainder
        .checked_add(fractional_whole)
        .ok_or(ErrorCode::MarketMathOverflow)?
        / denominator_scale;
    match whole.checked_add(correction) {
        Some(value) => Ok((value, false)),
        None => Ok((u128::MAX, true)),
    }
}

/// Prepared Huberized divergence state for one input-reserve center. The
/// threshold is solved once, then every implicit-solver probe is constant-time.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PreparedDivergenceStatePotential {
    center_input_reserve_raw: u64,
    coefficient_nad: u64,
    marginal_cap_nad: u64,
    huber_threshold_outward_raw: u64,
    threshold_potential_raw: u128,
    threshold_potential_saturated: bool,
}

impl PreparedDivergenceStatePotential {
    pub(crate) fn new(center_input_reserve_raw: u64, coefficient_nad: u64, marginal_cap_nad: u64) -> Result<Self> {
        require!(center_input_reserve_raw > 0, ErrorCode::InvalidArgument);
        let maximum_outward = u64::MAX - center_input_reserve_raw;
        let huber_threshold_outward_raw = if coefficient_nad == 0 || marginal_cap_nad == 0 {
            0
        } else if uncapped_divergence_marginal_rate_raw_nad(center_input_reserve_raw, maximum_outward, coefficient_nad)?
            <= marginal_cap_nad
        {
            maximum_outward
        } else {
            let mut low = 0_u64;
            let mut high = maximum_outward;
            while low < high {
                let width = high - low;
                let midpoint = low + width / 2 + width % 2;
                if uncapped_divergence_marginal_rate_raw_nad(center_input_reserve_raw, midpoint, coefficient_nad)?
                    <= marginal_cap_nad
                {
                    low = midpoint;
                } else {
                    high = midpoint - 1;
                }
            }
            low
        };
        let (threshold_potential_raw, threshold_potential_saturated) =
            uncapped_divergence_state_potential_raw_saturating(
                huber_threshold_outward_raw,
                center_input_reserve_raw,
                coefficient_nad,
            )?;
        Ok(Self {
            center_input_reserve_raw,
            coefficient_nad,
            marginal_cap_nad,
            huber_threshold_outward_raw,
            threshold_potential_raw,
            threshold_potential_saturated,
        })
    }

    pub(crate) fn state_potential(self, outward_coordinate_raw: u64) -> Result<(u128, bool)> {
        if outward_coordinate_raw == 0 || self.coefficient_nad == 0 || self.marginal_cap_nad == 0 {
            return Ok((0, false));
        }
        require!(
            outward_coordinate_raw <= u64::MAX - self.center_input_reserve_raw,
            ErrorCode::ReserveOverflow
        );
        if outward_coordinate_raw <= self.huber_threshold_outward_raw {
            return uncapped_divergence_state_potential_raw_saturating(
                outward_coordinate_raw,
                self.center_input_reserve_raw,
                self.coefficient_nad,
            );
        }
        if self.threshold_potential_saturated {
            return Ok((u128::MAX, true));
        }
        let tail = (outward_coordinate_raw - self.huber_threshold_outward_raw) as u128;
        let tail_potential = tail
            .checked_mul(self.marginal_cap_nad as u128)
            .and_then(|value| value.checked_div(NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        match self.threshold_potential_raw.checked_add(tail_potential) {
            Some(value) => Ok((value, false)),
            None => Ok((u128::MAX, true)),
        }
    }
}

/// One-shot gross-path toxicity charge from a capped state potential. The
/// caller freezes this result before producing the executable net quote.
pub(crate) fn gross_path_divergence_fee_raw(
    center_input_reserve_raw: u64,
    start_input_reserve_raw: u64,
    gross_end_input_reserve_raw: u64,
    coefficient_nad: u64,
    divergence_fee_share_cap_bps: u16,
) -> Result<(u128, bool)> {
    require!(
        gross_end_input_reserve_raw >= start_input_reserve_raw,
        ErrorCode::InvalidArgument
    );
    if coefficient_nad == 0 || divergence_fee_share_cap_bps == 0 {
        return Ok((0, false));
    }
    let start_outward = start_input_reserve_raw.saturating_sub(center_input_reserve_raw);
    let end_outward = gross_end_input_reserve_raw.saturating_sub(center_input_reserve_raw);
    if end_outward <= start_outward {
        return Ok((0, false));
    }
    let marginal_cap_nad = fee_share_cap_to_marginal_rate_nad(divergence_fee_share_cap_bps)?;
    let potential = PreparedDivergenceStatePotential::new(center_input_reserve_raw, coefficient_nad, marginal_cap_nad)?;
    let (start, start_saturated) = potential.state_potential(start_outward)?;
    let (end, end_saturated) = potential.state_potential(end_outward)?;
    if start_saturated || end_saturated {
        return Ok((u128::MAX, true));
    }
    Ok((end.checked_sub(start).ok_or(ErrorCode::FeeMathOverflow)?, false))
}

fn uncapped_divergence_marginal_rate_raw_nad(
    center_input_reserve_raw: u64,
    outward_coordinate_raw: u64,
    coefficient_nad: u64,
) -> Result<u64> {
    if outward_coordinate_raw == 0 || coefficient_nad == 0 {
        return Ok(0);
    }
    let outward = outward_coordinate_raw as u128;
    let endpoint = (center_input_reserve_raw as u128)
        .checked_add(outward)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let q48 = 1_u128 << 48;
    let outward_fraction_q48 = outward
        .checked_mul(q48)
        .and_then(|value| value.checked_div(endpoint))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let outward_squared_q48 = outward_fraction_q48
        .checked_mul(outward_fraction_q48)
        .ok_or(ErrorCode::MarketMathOverflow)?
        >> 48;
    let center_fraction_q48 = q48
        .checked_sub(outward_fraction_q48)
        .ok_or(ErrorCode::BrokenInvariant)?;
    require!(center_fraction_q48 > 0, ErrorCode::BrokenInvariant);
    let slope_factor_q48 = q48
        .checked_mul(3)
        .and_then(|value| value.checked_sub(outward_fraction_q48))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let shape_q48 = outward_squared_q48
        .checked_mul(slope_factor_q48)
        .and_then(|value| value.checked_div(center_fraction_q48))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let coefficient_times_four = (coefficient_nad as u128)
        .checked_mul(4)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let Some(numerator) = shape_q48.checked_mul(coefficient_times_four) else {
        return Ok(u64::MAX);
    };
    let rate = numerator
        .checked_div(q48.checked_mul(3).ok_or(ErrorCode::MarketMathOverflow)?)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(rate.min(u64::MAX as u128) as u64)
}

pub(crate) fn decay_volatility_nad(
    accumulator_nad: u64,
    last_update_slot: u64,
    current_slot: u64,
    half_life_ms: u64,
) -> Result<u64> {
    if accumulator_nad == 0 {
        return Ok(0);
    }
    if half_life_ms == 0 {
        return Ok(0);
    }
    let elapsed_slots = current_slot
        .checked_sub(last_update_slot)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let elapsed_ms = elapsed_slots
        .checked_mul(TARGET_MS_PER_SLOT)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let exponent = (elapsed_ms as u128)
        .saturating_mul(NATURAL_LOG_OF_TWO_NAD as u128)
        .checked_div(half_life_ms as u128)
        .unwrap_or(u128::MAX)
        .min(i64::MAX as u128) as i64;
    let alpha_nad = taylor_exp(-exponent, NAD, TAYLOR_TERMS) as u128;
    let decayed = (accumulator_nad as u128)
        .checked_mul(alpha_nad)
        .and_then(|value| value.checked_div(NAD as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(decayed).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

/// Produces the accumulator candidate for a successful price-changing swap.
///
/// Calling this function does not mutate state. The caller must commit the
/// result only after all swap checks and transfers succeed.
pub(crate) fn volatility_after_success_nad(
    decayed_accumulator_nad: u64,
    start_price_nad: u64,
    end_price_nad: u64,
    shock_cap_nad: u64,
    accumulator_cap_nad: u64,
) -> Result<u64> {
    require!(shock_cap_nad <= accumulator_cap_nad, ErrorCode::InvalidArgument);
    if start_price_nad == end_price_nad {
        return Ok(decayed_accumulator_nad.min(accumulator_cap_nad));
    }
    require!(start_price_nad > 0 && end_price_nad > 0, ErrorCode::InvalidArgument);
    let high = start_price_nad.max(end_price_nad) as u128;
    let low = start_price_nad.min(end_price_nad) as u128;
    let move_nad = ceil_div(high.checked_mul(NAD as u128).ok_or(ErrorCode::MarketMathOverflow)?, low)
        .and_then(|ratio_nad| ratio_nad.checked_sub(NAD as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let capped_shock = move_nad.min(shock_cap_nad as u128) as u64;
    Ok(decayed_accumulator_nad
        .checked_add(capped_shock)
        .ok_or(ErrorCode::MarketMathOverflow)?
        .min(accumulator_cap_nad))
}

pub(crate) fn effective_rate_floor_nad(amount: u64, total_amount: u64) -> Result<u64> {
    if amount == 0 {
        return Ok(0);
    }
    require!(total_amount > 0 && amount < total_amount, ErrorCode::InvalidArgument);
    let rate = (amount as u128)
        .checked_mul(NAD as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?
        / total_amount as u128;
    let rate = u64::try_from(rate).map_err(|_| ErrorCode::MarketMathOverflow)?;
    require!(rate < NAD, ErrorCode::BrokenInvariant);
    Ok(rate)
}

/// Converts non-negative pressure `signal * coefficient` into the asymptotic
/// fee rate `pressure / (1 + pressure)`. It is linear near zero, monotonically
/// increasing, and strictly below 100% for every finite input. Production
/// signal/coefficient bounds limit pressure; the component gross budget is
/// still the authoritative volatility-fee ceiling.
pub(crate) fn asymptotic_scaled_rate_nad(signal_nad: u128, coefficient_nad: u64) -> Result<u64> {
    if signal_nad == 0 || coefficient_nad == 0 {
        return Ok(0);
    }

    // If p = signal*coefficient/NAD^2, rate = NAD*p/(1+p). Evaluating the
    // complementary distance from 100% avoids the potentially 165-bit
    // `pressure*NAD` product:
    //
    // rate = NAD - ceil(NAD^3 / (NAD^2 + signal*coefficient)).
    let Some(pressure_numerator) = signal_nad.checked_mul(coefficient_nad as u128) else {
        return Ok(NAD - 1);
    };
    let nad_squared = (NAD as u128)
        .checked_mul(NAD as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let Some(denominator) = nad_squared.checked_add(pressure_numerator) else {
        return Ok(NAD - 1);
    };
    let nad_cubed = nad_squared
        .checked_mul(NAD as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let complement = if nad_cubed == 0 {
        0
    } else {
        (nad_cubed - 1) / denominator + 1
    };
    let rate = (NAD as u128)
        .checked_sub(complement)
        .ok_or(ErrorCode::BrokenInvariant)?;
    u64::try_from(rate).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
include!("../../tests/support/amm_fees.rs");

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/amm_fees.rs");
}
