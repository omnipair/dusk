#[allow(clippy::assign_op_pattern, clippy::manual_div_ceil)]
mod wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U512(8);
    }
}

use wide::U512;

const Q48: u128 = 1_u128 << 48;

const MAX_EUCLID_GCD_ITERATIONS: usize = 256;

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

/// Symmetric relative distance: `max(a / b, b / a) - 1`, rounded up.
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
fn mul_div_rem_saturating(value: u128, numerator: u128, denominator: u128) -> Result<(u128, u128, bool)> {
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

/// NAD-scaled continuous marginal rate of the divergence potential at one
/// executable input-reserve coordinate.
///
/// This is the derivative of the divergence state potential. It is exposed
/// only inside the crate so the swap engine can solve the fee-adjusted endpoint
/// without duplicating the protocol formula. Values beyond `u64` are
/// deliberately saturated: the marginal is only a Newton accelerator, while
/// exact potential costs remain the authoritative feasibility proof.
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
    for _ in 0..MAX_EUCLID_GCD_ITERATIONS {
        if b == 0 {
            break;
        }
        (a, b) = (b, a % b);
    }
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

fn u512_to_u128(value: U512) -> Result<u128> {
    require!(value <= U512::from(u128::MAX), ErrorCode::MarketMathOverflow);
    Ok(value.as_u128())
}

/// Quotes a deterministic fee from frozen pre-state and a simulated endpoint.
///
/// The volatility charge uses the accumulator before the current move. The
/// returned post-success value is therefore safe for a two-pass quote/execute
/// flow: both passes freeze `pre_state`, and execution commits it only on
/// success.
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
    let divergence_signal = if config.divergence_coefficient_nad == 0 {
        0
    } else {
        path.divergence_surcharge_amount
    };
    let mut quote = super::output_denominated_dynamic_fee(
        path.amount_in,
        path.amount_in,
        divergence_signal,
        decayed_volatility_nad,
        config.base_fee_bps,
        config,
    )?;
    quote.post_success_volatility_nad = volatility_after_success_nad(
        decayed_volatility_nad,
        path.start_price_nad,
        path.end_price_nad,
        config.volatility_shock_cap_nad,
        config.volatility_accumulator_cap_nad,
    )?;
    Ok(quote)
}

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

impl PreparedDivergenceStatePotential {
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

    pub(crate) const fn huber_threshold_outward_raw(self) -> u64 {
        self.huber_threshold_outward_raw
    }
}
