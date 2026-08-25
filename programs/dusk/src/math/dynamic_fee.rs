use anchor_lang::prelude::*;

use crate::{
    constants::{
        BPS_DENOMINATOR, MAX_PARAMETER_FEE_BPS, NAD, NATURAL_LOG_OF_TWO_NAD, TARGET_MS_PER_SLOT, TAYLOR_TERMS,
    },
    errors::ErrorCode,
    math::{ceil_div, taylor_exp},
};

#[cfg(test)]
use crate::constants::NAD_DECIMALS;

#[cfg(test)]
#[allow(clippy::assign_op_pattern, clippy::manual_div_ceil)]
mod wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U512(8);
    }
}

#[cfg(test)]
use wide::U512;

const Q64: u128 = 1_u128 << 64;
#[cfg(test)]
const Q48: u128 = 1_u128 << 48;
#[cfg(test)]
const MAX_EUCLID_GCD_ITERATIONS: usize = 256;

#[cfg(test)]
thread_local! {
    static LAST_MUL_DIV_FALLBACK_ITERATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static LAST_GCD_ITERATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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

pub(crate) const fn minimum_executable_input(gross_input: u64) -> u64 {
    gross_input - hard_total_fee_budget_floor(gross_input)
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

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicFeePath {
    pub amount_in: u64,
    pub start_price_nad: u64,
    pub end_price_nad: u64,
    pub current_slot: u64,
    /// Additive outward-distance fee potential, already expressed in raw
    /// input-token units. The swap engine derives it from the invariant's
    /// input-reserve coordinate before the final curve quote.
    pub divergence_surcharge_amount: u64,
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

/// Symmetric relative distance: `max(a / b, b / a) - 1`, rounded up.
#[cfg(test)]
pub(crate) fn symmetric_ratio_distance_nad(a: u64, b: u64) -> Result<u128> {
    require!(a > 0 && b > 0, ErrorCode::InvalidArgument);
    let high = a.max(b) as u128;
    let low = a.min(b) as u128;
    let ratio_nad = ceil_div(high.checked_mul(NAD as u128).ok_or(ErrorCode::MarketMathOverflow)?, low)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    ratio_nad
        .checked_sub(NAD as u128)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

/// Additive Dusk divergence-fee potential over the invariant's input-reserve
/// coordinate.
///
/// `center_input_reserve_nad` is the balanced reserve on the current invariant
/// (`D / 2` in quote units, or `D / (2 * center)` in base units). An exact-in
/// swap always increases its input reserve, so:
///
/// - below the balanced reserve, flow is restorative and free;
/// - a crossing charges only the coordinate beyond the balanced reserve;
/// - already-outward flow charges `F(end) - F(start)`.
///
/// Near the center, `F` integrates the original quadratic marginal surcharge.
/// Once that marginal reaches the configured fee-share cap transformed into
/// executable-input terms, the state potential continues linearly. Every
/// monotonic segment remains a difference of this one Huberized potential, so
/// split paths telescope instead of depending on a final-quote clamp.
#[cfg(test)]
pub(crate) fn outward_divergence_fee_potential_nad(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    end_input_reserve_nad: u128,
    coefficient_nad: u64,
    divergence_fee_share_cap_bps: u16,
) -> Result<u128> {
    require!(center_input_reserve_nad > 0, ErrorCode::InvalidArgument);
    require!(
        end_input_reserve_nad >= start_input_reserve_nad,
        ErrorCode::InvalidArgument
    );
    let marginal_cap_nad = fee_share_cap_to_marginal_rate_nad(divergence_fee_share_cap_bps)?;
    if coefficient_nad == 0 || marginal_cap_nad == 0 {
        return Ok(0);
    }

    let start_outward = start_input_reserve_nad.saturating_sub(center_input_reserve_nad);
    let end_outward = end_input_reserve_nad.saturating_sub(center_input_reserve_nad);
    if end_outward <= start_outward {
        return Ok(0);
    }

    let start_potential = divergence_state_potential_u512_reference(
        start_outward,
        center_input_reserve_nad,
        coefficient_nad,
        marginal_cap_nad,
    )?;
    let end_potential = divergence_state_potential_u512_reference(
        end_outward,
        center_input_reserve_nad,
        coefficient_nad,
        marginal_cap_nad,
    )?;
    let fee = end_potential
        .checked_sub(start_potential)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u512_to_u128(fee)
}

#[cfg(test)]
fn divergence_state_potential_u512_reference(
    outward_coordinate_nad: u128,
    center_input_reserve_nad: u128,
    coefficient_nad: u64,
    marginal_cap_nad: u64,
) -> Result<U512> {
    if outward_coordinate_nad == 0 || coefficient_nad == 0 || marginal_cap_nad == 0 {
        return Ok(U512::zero());
    }
    if divergence_marginal_is_at_most_cap_u512(
        outward_coordinate_nad,
        center_input_reserve_nad,
        coefficient_nad,
        marginal_cap_nad,
    )? {
        return uncapped_divergence_state_potential_u512_reference(
            outward_coordinate_nad,
            center_input_reserve_nad,
            coefficient_nad,
        );
    }

    let mut low = 0_u128;
    let mut high = outward_coordinate_nad;
    while low < high {
        let midpoint = low + (high - low) / 2 + (high - low) % 2;
        if divergence_marginal_is_at_most_cap_u512(
            midpoint,
            center_input_reserve_nad,
            coefficient_nad,
            marginal_cap_nad,
        )? {
            low = midpoint;
        } else {
            high = midpoint - 1;
        }
    }
    let threshold_potential =
        uncapped_divergence_state_potential_u512_reference(low, center_input_reserve_nad, coefficient_nad)?;
    let tail = U512::from(outward_coordinate_nad - low)
        .checked_mul(U512::from(marginal_cap_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?
        / U512::from(NAD);
    threshold_potential
        .checked_add(tail)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
fn uncapped_divergence_state_potential_u512_reference(
    outward_coordinate_nad: u128,
    center_input_reserve_nad: u128,
    coefficient_nad: u64,
) -> Result<U512> {
    let outward = U512::from(outward_coordinate_nad);
    let center = U512::from(center_input_reserve_nad);
    let coefficient_times_four = U512::from(coefficient_nad)
        .checked_mul(U512::from(4_u8))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let numerator = outward
        .checked_mul(outward)
        .and_then(|value| value.checked_mul(outward))
        .and_then(|value| value.checked_mul(coefficient_times_four))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator = center
        .checked_mul(center.checked_add(outward).ok_or(ErrorCode::MarketMathOverflow)?)
        .and_then(|value| value.checked_mul(U512::from(NAD)))
        .and_then(|value| value.checked_mul(U512::from(3_u8)))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(!denominator.is_zero(), ErrorCode::InvalidArgument);
    Ok(numerator / denominator)
}

#[cfg(test)]
fn divergence_marginal_is_at_most_cap_u512(
    outward_coordinate_nad: u128,
    center_input_reserve_nad: u128,
    coefficient_nad: u64,
    marginal_cap_nad: u64,
) -> Result<bool> {
    if outward_coordinate_nad == 0 || coefficient_nad == 0 {
        return Ok(true);
    }
    let outward = U512::from(outward_coordinate_nad);
    let center = U512::from(center_input_reserve_nad);
    let endpoint = center.checked_add(outward).ok_or(ErrorCode::MarketMathOverflow)?;
    let numerator = U512::from(coefficient_nad)
        .checked_mul(U512::from(4_u8))
        .and_then(|value| value.checked_mul(outward))
        .and_then(|value| value.checked_mul(outward))
        .and_then(|value| {
            center
                .checked_mul(U512::from(3_u8))
                .and_then(|three_center| {
                    outward
                        .checked_mul(U512::from(2_u8))
                        .and_then(|two_outward| three_center.checked_add(two_outward))
                })
                .and_then(|shape| value.checked_mul(shape))
        })
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator = U512::from(marginal_cap_nad)
        .checked_mul(U512::from(3_u8))
        .and_then(|value| value.checked_mul(center))
        .and_then(|value| value.checked_mul(endpoint))
        .and_then(|value| value.checked_mul(endpoint))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(numerator <= denominator)
}

/// Test-reference `value * numerator / denominator` for a u128 quotient, plus
/// its remainder. Production uses formulas specialized to bounded raw-token
/// coordinates; this generic fallback exists only to verify their arithmetic.
#[cfg(test)]
fn mul_div_rem_saturating(value: u128, numerator: u128, denominator: u128) -> Result<(u128, u128, bool)> {
    #[cfg(test)]
    LAST_MUL_DIV_FALLBACK_ITERATIONS.with(|iterations| iterations.set(0));
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    if value == 0 || numerator == 0 {
        return Ok((0, 0, false));
    }
    if let Some(product) = value.checked_mul(numerator) {
        return Ok((product / denominator, product % denominator, false));
    }

    let Some(base_quotient) = (value / denominator).checked_mul(numerator) else {
        return Ok((u128::MAX, 0, true));
    };
    let addend = value % denominator;
    let mut quotient = 0_u128;
    let mut remainder = 0_u128;
    let significant_bits = u128::BITS - numerator.leading_zeros();
    #[cfg(test)]
    LAST_MUL_DIV_FALLBACK_ITERATIONS.with(|iterations| iterations.set(significant_bits));
    for bit in (0..significant_bits).rev() {
        let carry = if remainder >= denominator - remainder {
            remainder -= denominator - remainder;
            1_u128
        } else {
            remainder += remainder;
            0_u128
        };
        let Some(next) = quotient.checked_mul(2).and_then(|result| result.checked_add(carry)) else {
            return Ok((u128::MAX, 0, true));
        };
        quotient = next;

        if (numerator >> bit) & 1 == 1 {
            let carry = if remainder >= denominator - addend {
                remainder -= denominator - addend;
                1_u128
            } else {
                remainder += addend;
                0_u128
            };
            let Some(next) = quotient.checked_add(carry) else {
                return Ok((u128::MAX, 0, true));
            };
            quotient = next;
        }
    }
    let Some(quotient) = base_quotient.checked_add(quotient) else {
        return Ok((u128::MAX, 0, true));
    };
    Ok((quotient, remainder, false))
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
#[cfg(test)]
pub(crate) struct PreparedDivergenceStatePotential {
    center_input_reserve_raw: u64,
    coefficient_nad: u64,
    marginal_cap_nad: u64,
    huber_threshold_outward_raw: u64,
    threshold_potential_raw: u128,
    threshold_potential_saturated: bool,
}

#[cfg(test)]
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

    pub(crate) fn marginal_rate_nad(self, outward_coordinate_raw: u64) -> Result<u64> {
        if self.coefficient_nad == 0 || self.marginal_cap_nad == 0 {
            return Ok(0);
        }
        if outward_coordinate_raw >= self.huber_threshold_outward_raw {
            return Ok(self.marginal_cap_nad);
        }
        Ok(uncapped_divergence_marginal_rate_raw_nad(
            self.center_input_reserve_raw,
            outward_coordinate_raw,
            self.coefficient_nad,
        )?
        .min(self.marginal_cap_nad))
    }

    #[cfg(test)]
    pub(crate) const fn huber_threshold_outward_raw(self) -> u64 {
        self.huber_threshold_outward_raw
    }
}

/// One-shot gross-path toxicity charge. Unlike the legacy implicit fee solve,
/// this evaluates the already-quoted gross endpoint once; the caller freezes
/// the result before producing the executable net quote.
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
    // The explicit curve freezes toxicity from the provisional gross path and
    // caps the resulting component once against gross input. It therefore
    // does not need the legacy Huber-threshold search: evaluate the analytic
    // potential at both endpoints directly and let the caller apply the
    // configured component/total budgets.
    let (start, start_saturated) =
        uncapped_divergence_state_potential_raw_saturating(start_outward, center_input_reserve_raw, coefficient_nad)?;
    let (end, end_saturated) =
        uncapped_divergence_state_potential_raw_saturating(end_outward, center_input_reserve_raw, coefficient_nad)?;
    if start_saturated || end_saturated {
        return Ok((u128::MAX, true));
    }
    Ok((end.checked_sub(start).ok_or(ErrorCode::FeeMathOverflow)?, false))
}

#[cfg(test)]
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

/// Frozen coordinates for repeated exact-cost probes of one implicit
/// divergence solve. Decimal normalization is removed when a raw endpoint is
/// evaluated, keeping the hot potential arithmetic in u64/u128.
#[derive(Clone, Copy)]
#[cfg(test)]
pub(crate) struct PreparedOutwardDivergencePotential {
    pub(crate) center_input_reserve_nad: u128,
    pub(crate) start_input_reserve_nad: u128,
    pub(crate) coefficient_nad: u64,
    pub(crate) divergence_fee_share_cap_bps: u16,
}

#[cfg(test)]
pub(crate) fn prepare_outward_divergence_potential(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    coefficient_nad: u64,
    divergence_fee_share_cap_bps: u16,
) -> Result<PreparedOutwardDivergencePotential> {
    require!(center_input_reserve_nad > 0, ErrorCode::InvalidArgument);
    fee_share_cap_to_marginal_rate_nad(divergence_fee_share_cap_bps)?;
    Ok(PreparedOutwardDivergencePotential {
        center_input_reserve_nad,
        start_input_reserve_nad,
        coefficient_nad,
        divergence_fee_share_cap_bps,
    })
}

/// Returns the additive divergence potential in raw-token units, saturating
/// only when the exact value cannot fit `u128`. The swap solver uses the
/// saturation bit solely to classify a probe as certainly unaffordable; the
/// selected feasible endpoint is always recomputed below the saturation
/// boundary before it can be charged.
#[cfg(test)]
pub(crate) fn outward_divergence_fee_raw_saturating_prepared(
    prepared: &PreparedOutwardDivergencePotential,
    end_input_reserve_nad: u128,
    input_decimals: u8,
) -> Result<(u128, bool)> {
    require!(
        end_input_reserve_nad >= prepared.start_input_reserve_nad,
        ErrorCode::InvalidArgument
    );
    let marginal_cap_nad = fee_share_cap_to_marginal_rate_nad(prepared.divergence_fee_share_cap_bps)?;
    if prepared.coefficient_nad == 0 || marginal_cap_nad == 0 {
        return Ok((0, false));
    }
    require!(input_decimals <= NAD_DECIMALS, ErrorCode::UnsupportedAssetDecimals);
    let decimal_scale = 10_u128
        .checked_pow((NAD_DECIMALS - input_decimals) as u32)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Round the possibly fractional invariant center outward. This never
    // charges restorative flow and moves the boundary by less than one raw
    // token atom.
    let Some(center_raw) =
        ceil_div(prepared.center_input_reserve_nad, decimal_scale).and_then(|value| u64::try_from(value).ok())
    else {
        // A balanced input reserve beyond the complete u64 token-account
        // domain cannot be crossed by any executable trade. Every reachable
        // endpoint is therefore restorative, not an overflowing outward fee.
        return Ok((0, false));
    };
    let Ok(start_raw) = u64::try_from(prepared.start_input_reserve_nad / decimal_scale) else {
        return Ok((u128::MAX, true));
    };
    let Ok(end_raw) = u64::try_from(end_input_reserve_nad / decimal_scale) else {
        return Ok((u128::MAX, true));
    };
    let start_outward = start_raw.saturating_sub(center_raw);
    let end_outward = end_raw.saturating_sub(center_raw);
    if end_outward <= start_outward {
        return Ok((0, false));
    }
    let state = PreparedDivergenceStatePotential::new(center_raw, prepared.coefficient_nad, marginal_cap_nad)?;
    let (start, start_saturated) = state.state_potential(start_outward)?;
    let (end, end_saturated) = state.state_potential(end_outward)?;
    if start_saturated || end_saturated {
        return Ok((u128::MAX, true));
    }
    Ok((end.checked_sub(start).ok_or(ErrorCode::MarketMathOverflow)?, false))
}

#[cfg(test)]
pub(crate) fn outward_divergence_fee_raw_saturating(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    end_input_reserve_nad: u128,
    input_decimals: u8,
    coefficient_nad: u64,
    divergence_fee_share_cap_bps: u16,
) -> Result<(u128, bool)> {
    let prepared = prepare_outward_divergence_potential(
        center_input_reserve_nad,
        start_input_reserve_nad,
        coefficient_nad,
        divergence_fee_share_cap_bps,
    )?;
    outward_divergence_fee_raw_saturating_prepared(&prepared, end_input_reserve_nad, input_decimals)
}

/// NAD-scaled continuous marginal rate of the divergence potential at one
/// executable input-reserve coordinate.
///
/// This is the derivative of the divergence state potential. It is exposed
/// only inside the crate so the swap engine can solve the fee-adjusted endpoint
/// without duplicating the protocol formula. Values beyond `u64` are
/// deliberately saturated: the marginal is only a Newton accelerator, while
/// exact potential costs remain the authoritative feasibility proof.
#[cfg(test)]
pub(crate) fn outward_divergence_marginal_rate_nad(
    center_input_reserve_nad: u128,
    input_reserve_nad: u128,
    coefficient_nad: u64,
    divergence_fee_share_cap_bps: u16,
) -> Result<u64> {
    require!(center_input_reserve_nad > 0, ErrorCode::InvalidArgument);
    let marginal_cap_nad = fee_share_cap_to_marginal_rate_nad(divergence_fee_share_cap_bps)?;
    if coefficient_nad == 0 || marginal_cap_nad == 0 || input_reserve_nad <= center_input_reserve_nad {
        return Ok(0);
    }

    let mut center = center_input_reserve_nad;
    let mut outward = input_reserve_nad
        .checked_sub(center_input_reserve_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // The production coordinates share their decimal normalization factor.
    // Removing the full gcd keeps the derivative scale-invariant and normally
    // restores raw-u64 bounds before any multiplication.
    let (mut a, mut b) = (center, outward);
    let mut _gcd_iterations = 0_usize;
    for iteration in 0..MAX_EUCLID_GCD_ITERATIONS {
        if b == 0 {
            break;
        }
        (a, b) = (b, a % b);
        _gcd_iterations = iteration + 1;
    }
    #[cfg(test)]
    LAST_GCD_ITERATIONS.with(|iterations| iterations.set(_gcd_iterations));
    require_eq!(b, 0, ErrorCode::MarketMathOverflow);
    center /= a;
    outward /= a;
    let endpoint = center.checked_add(outward).ok_or(ErrorCode::MarketMathOverflow)?;
    let (outward_fraction_q48, _, saturated) = mul_div_rem_saturating(outward, Q48, endpoint)?;
    if saturated {
        return Ok(u64::MAX);
    }
    let outward_squared_q48 = outward_fraction_q48
        .checked_mul(outward_fraction_q48)
        .ok_or(ErrorCode::MarketMathOverflow)?
        >> 48;
    let center_fraction_q48 = Q48
        .checked_sub(outward_fraction_q48)
        .ok_or(ErrorCode::BrokenInvariant)?;
    require!(center_fraction_q48 > 0, ErrorCode::BrokenInvariant);
    let slope_factor_q48 = Q48
        .checked_mul(3)
        .and_then(|value| value.checked_sub(outward_fraction_q48))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let (shape_q48, _, saturated) = mul_div_rem_saturating(outward_squared_q48, slope_factor_q48, center_fraction_q48)?;
    if saturated {
        return Ok(u64::MAX);
    }
    let coefficient_times_four = (coefficient_nad as u128)
        .checked_mul(4)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator = Q48.checked_mul(3).ok_or(ErrorCode::MarketMathOverflow)?;
    let (rate, _, saturated) = mul_div_rem_saturating(shape_q48, coefficient_times_four, denominator)?;
    Ok(if saturated || rate > u64::MAX as u128 {
        u64::MAX
    } else {
        rate as u64
    }
    .min(marginal_cap_nad))
}

#[cfg(test)]
fn u512_to_u128(value: U512) -> Result<u128> {
    require!(value <= U512::from(u128::MAX), ErrorCode::MarketMathOverflow);
    Ok(value.as_u128())
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

/// Quotes a deterministic fee from frozen pre-state and a simulated endpoint.
///
/// The volatility charge uses the accumulator before the current move. The
/// returned post-success value is therefore safe for a two-pass quote/execute
/// flow: both passes freeze `pre_state`, and execution commits it only on
/// success.
#[cfg(test)]
pub(crate) fn quote_dynamic_fee(
    config: DynamicFeeConfig,
    pre_state: DynamicFeePreState,
    path: DynamicFeePath,
) -> Result<DynamicFeeQuote> {
    validate_fee_share_caps(
        config.base_fee_bps,
        config.divergence_fee_share_cap_bps,
        config.volatility_fee_share_cap_bps,
    )?;
    require!(
        config.volatility_shock_cap_nad <= config.volatility_accumulator_cap_nad,
        ErrorCode::InvalidArgument
    );
    require!(
        config.volatility_coefficient_nad == 0 || config.volatility_half_life_ms > 0,
        ErrorCode::InvalidHalfLife
    );
    require!(path.amount_in > 0, ErrorCode::AmountZero);

    let decayed_volatility_nad = decay_volatility_nad(
        pre_state
            .volatility_accumulator_nad
            .min(config.volatility_accumulator_cap_nad),
        pre_state.volatility_last_update_slot,
        path.current_slot,
        config.volatility_half_life_ms,
    )?;
    let hard_total_budget = hard_total_fee_budget_floor(path.amount_in);
    let base_fee_amount = gross_fee_budget_floor(path.amount_in, config.base_fee_bps)?;
    let after_base = path
        .amount_in
        .checked_sub(base_fee_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(after_base > 0, ErrorCode::InsufficientOutputAmount);

    // Volatility pressure is naturally bounded below 100% and is applied only
    // to input left after the base fee. Floor rounding preserves at least one
    // atom for every finite rate.
    let signal_nad = decayed_volatility_nad as u128;
    let coefficient_nad = config.volatility_coefficient_nad;
    let volatility_marginal_rate_nad = asymptotic_scaled_rate_nad(signal_nad, coefficient_nad)?;
    require!(volatility_marginal_rate_nad < NAD, ErrorCode::InvalidSwapFeeBps);
    let uncapped_volatility_surcharge = u64::try_from(
        (after_base as u128)
            .checked_mul(volatility_marginal_rate_nad as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?
            / NAD as u128,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let volatility_component_budget = gross_fee_budget_floor(path.amount_in, config.volatility_fee_share_cap_bps)?;
    let remaining_total_budget = hard_total_budget
        .checked_sub(base_fee_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    let volatility_surcharge_amount = uncapped_volatility_surcharge
        .min(volatility_component_budget)
        .min(remaining_total_budget);
    require!(volatility_surcharge_amount < after_base, ErrorCode::BrokenInvariant);
    let after_volatility = after_base
        .checked_sub(volatility_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(after_volatility > 0, ErrorCode::InsufficientOutputAmount);

    // The swap engine computes this additive potential over exactly the
    // post-base, post-volatility input coordinate. It must leave positive
    // raw input. The curve subsequently requires that input to remain positive
    // after NAD normalization. A rejection is token-granularity arithmetic
    // safety, not a configurable economic fee ceiling.
    let divergence_surcharge_amount = if config.divergence_coefficient_nad == 0 {
        0
    } else {
        path.divergence_surcharge_amount
    };
    let divergence_component_budget = gross_fee_budget_floor(path.amount_in, config.divergence_fee_share_cap_bps)?;
    let remaining_total_budget = hard_total_budget
        .checked_sub(base_fee_amount)
        .and_then(|value| value.checked_sub(volatility_surcharge_amount))
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(
        divergence_surcharge_amount <= divergence_component_budget
            && divergence_surcharge_amount <= remaining_total_budget,
        ErrorCode::InvalidSwapFeeBps
    );
    require!(
        divergence_surcharge_amount < after_volatility,
        ErrorCode::InsufficientOutputAmount
    );
    let amount_in_for_quote = after_volatility
        .checked_sub(divergence_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(amount_in_for_quote > 0, ErrorCode::InsufficientOutputAmount);

    let dynamic_surcharge_amount = divergence_surcharge_amount
        .checked_add(volatility_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    let total_fee_amount = base_fee_amount
        .checked_add(dynamic_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(total_fee_amount <= hard_total_budget, ErrorCode::BrokenInvariant);
    require_gte!(
        amount_in_for_quote,
        minimum_executable_input(path.amount_in),
        ErrorCode::BrokenInvariant
    );

    // Report component rates against gross input so they remain additive. Floor
    // rounding can understate the amount ratio by less than one NAD atom per
    // component, but can never report a total at or above 100%.
    let base_rate_nad = effective_rate_floor_nad(base_fee_amount, path.amount_in)?;
    let divergence_rate_nad = effective_rate_floor_nad(divergence_surcharge_amount, path.amount_in)?;
    let volatility_rate_nad = effective_rate_floor_nad(volatility_surcharge_amount, path.amount_in)?;
    let total_rate_nad = base_rate_nad
        .checked_add(divergence_rate_nad)
        .and_then(|rate| rate.checked_add(volatility_rate_nad))
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(total_rate_nad < NAD, ErrorCode::BrokenInvariant);
    let post_success_volatility_nad = volatility_after_success_nad(
        decayed_volatility_nad,
        path.start_price_nad,
        path.end_price_nad,
        config.volatility_shock_cap_nad,
        config.volatility_accumulator_cap_nad,
    )?;

    Ok(DynamicFeeQuote {
        base_rate_nad,
        divergence_rate_nad,
        volatility_rate_nad,
        total_rate_nad,
        base_fee_amount,
        divergence_surcharge_amount,
        volatility_surcharge_amount,
        dynamic_surcharge_amount,
        total_fee_amount,
        decayed_volatility_nad,
        post_success_volatility_nad,
    })
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

#[cfg(test)]
pub(crate) fn fee_amount_ceil(amount: u64, fee_rate_nad: u64) -> Result<u64> {
    require!(fee_rate_nad <= NAD, ErrorCode::InvalidSwapFeeBps);
    let fee = ceil_div(
        (amount as u128)
            .checked_mul(fee_rate_nad as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        NAD as u128,
    )
    .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(fee).map_err(|_| ErrorCode::MarketMathOverflow.into())
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
mod tests {
    include!("../tests/math/dynamic_fee.rs");
}
