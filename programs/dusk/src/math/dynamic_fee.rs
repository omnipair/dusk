use anchor_lang::prelude::*;

use crate::{
    constants::{NAD, NAD_DECIMALS, TARGET_MS_PER_SLOT},
    errors::ErrorCode,
    shared::math::ceil_div,
};

use super::{exponential_price_decay, normalize_to_nad, MAX_COMMON_RESERVE};

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

/// All fee rates and signals use NAD precision (`NAD == 100%`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicFeeConfig {
    pub base_fee_rate_nad: u64,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicFeePreState {
    /// Frozen concentration center used for every pass of the quote.
    pub center_price_nad: u64,
    /// Accumulator as committed by the last successful swap.
    pub volatility_accumulator_nad: u64,
    pub volatility_last_update_slot: u64,
}

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

impl DynamicFeeConfig {
    fn validate(&self) -> Result<()> {
        require!(self.base_fee_rate_nad < NAD, ErrorCode::InvalidSwapFeeBps);
        require!(
            self.volatility_shock_cap_nad <= self.volatility_accumulator_cap_nad,
            ErrorCode::InvalidArgument
        );
        require!(
            self.volatility_coefficient_nad == 0 || self.volatility_half_life_ms > 0,
            ErrorCode::InvalidHalfLife
        );
        Ok(())
    }
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
/// Near the center, `F` integrates a quadratic marginal surcharge. Farther
/// out, its marginal rate grows without a protocol fee cap, so the share of
/// gross input paid as divergence toll can approach 100% while every accepted
/// swap still leaves positive executable input. Because every monotonic segment
/// is a difference of the same state potential, split paths telescope instead
/// of depending on an arbitrary price-coordinate average.
#[cfg(test)]
pub(crate) fn outward_divergence_fee_potential_nad(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    end_input_reserve_nad: u128,
    coefficient_nad: u64,
) -> Result<u128> {
    require!(center_input_reserve_nad > 0, ErrorCode::InvalidArgument);
    require!(
        end_input_reserve_nad >= start_input_reserve_nad,
        ErrorCode::InvalidArgument
    );
    if coefficient_nad == 0 {
        return Ok(0);
    }

    let start_outward = start_input_reserve_nad.saturating_sub(center_input_reserve_nad);
    let end_outward = end_input_reserve_nad.saturating_sub(center_input_reserve_nad);
    if end_outward <= start_outward {
        return Ok(0);
    }

    let start_potential = divergence_state_potential_wide(start_outward, center_input_reserve_nad, coefficient_nad)?;
    let end_potential = divergence_state_potential_wide(end_outward, center_input_reserve_nad, coefficient_nad)?;
    let fee = end_potential
        .checked_sub(start_potential)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u512_to_u128(fee)
}

/// Uncapped smooth rational potential:
///
/// `F(u) = 4*c*u^3 / [3*NAD*q0*(q0+u)]`.
///
/// Its marginal rate is zero at the center, strictly increasing, and
/// unbounded. Around the center it is
/// `4*c*(u/q0)^2/NAD`, preserving the coefficient's quadratic meaning. Far
/// from center the state potential grows quadratically, so additional outward
/// flow keeps deteriorating instead of approaching a hidden fixed fee rate.
fn divergence_state_potential_wide(
    outward_coordinate_nad: u128,
    center_input_reserve_nad: u128,
    coefficient_nad: u64,
) -> Result<U512> {
    if outward_coordinate_nad == 0 {
        return Ok(U512::zero());
    }

    // Ordinary executable reserves fit the same exact rational expression in
    // U256. Keep U512 as the checked fallback for the full documented u128
    // domain; selecting the narrow path cannot change division or rounding.
    if let Some(potential) =
        divergence_state_potential_u256(outward_coordinate_nad, center_input_reserve_nad, coefficient_nad)
    {
        return Ok(potential);
    }
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
    Ok(numerator / denominator)
}

fn divergence_state_potential_u256(
    outward_coordinate_nad: u128,
    center_input_reserve_nad: u128,
    coefficient_nad: u64,
) -> Option<U512> {
    let outward = U256::from(outward_coordinate_nad);
    let center = U256::from(center_input_reserve_nad);
    let coefficient_times_four = U256::from(coefficient_nad).checked_mul(U256::from(4_u8))?;
    let numerator = outward
        .checked_mul(outward)?
        .checked_mul(outward)?
        .checked_mul(coefficient_times_four)?;
    let denominator = center
        .checked_mul(center.checked_add(outward)?)?
        .checked_mul(U256::from(NAD))?
        .checked_mul(U256::from(3_u8))?;
    if denominator.is_zero() {
        return None;
    }
    let value = numerator / denominator;
    Some(U512([value.0[0], value.0[1], value.0[2], value.0[3], 0, 0, 0, 0]))
}

#[cfg(test)]
fn divergence_state_potential_u512_reference(
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

/// Frozen start state for repeated exact-cost probes of one implicit
/// divergence solve. The start potential contains a U512 division, so paying
/// for it once instead of once per secant/Newton probe materially reduces SBF
/// cost without changing any probe or rounding result.
#[derive(Clone, Copy)]
pub(crate) struct PreparedOutwardDivergencePotential {
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    start_outward_nad: u128,
    start_potential_nad: U512,
    coefficient_nad: u64,
}

impl PreparedOutwardDivergencePotential {
    pub(crate) const fn start_input_reserve_nad(&self) -> u128 {
        self.start_input_reserve_nad
    }
}

pub(crate) fn prepare_outward_divergence_potential(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    coefficient_nad: u64,
) -> Result<PreparedOutwardDivergencePotential> {
    require!(center_input_reserve_nad > 0, ErrorCode::InvalidArgument);
    let start_outward_nad = start_input_reserve_nad.saturating_sub(center_input_reserve_nad);
    let start_potential_nad =
        divergence_state_potential_wide(start_outward_nad, center_input_reserve_nad, coefficient_nad)?;
    Ok(PreparedOutwardDivergencePotential {
        center_input_reserve_nad,
        start_input_reserve_nad,
        start_outward_nad,
        start_potential_nad,
        coefficient_nad,
    })
}

/// Returns the additive divergence potential in raw-token units, saturating
/// only when the exact value cannot fit `u128`. The swap solver uses the
/// saturation bit solely to classify a probe as certainly unaffordable; the
/// selected feasible endpoint is always recomputed below the saturation
/// boundary before it can be charged.
pub(crate) fn outward_divergence_fee_raw_saturating_prepared(
    prepared: &PreparedOutwardDivergencePotential,
    end_input_reserve_nad: u128,
    input_decimals: u8,
) -> Result<(u128, bool)> {
    require!(
        end_input_reserve_nad >= prepared.start_input_reserve_nad,
        ErrorCode::InvalidArgument
    );
    if prepared.coefficient_nad == 0 {
        return Ok((0, false));
    }

    let end_outward = end_input_reserve_nad.saturating_sub(prepared.center_input_reserve_nad);
    if end_outward <= prepared.start_outward_nad {
        return Ok((0, false));
    }
    let end =
        divergence_state_potential_wide(end_outward, prepared.center_input_reserve_nad, prepared.coefficient_nad)?;
    let fee_nad = end
        .checked_sub(prepared.start_potential_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    denormalize_u512_ceil_saturating(fee_nad, input_decimals)
}

/// Fixed-`D` divergence context for positive-concentration pools. The state
/// potential is denominated directly in raw input atoms, so subtracting two
/// endpoints telescopes exactly across arbitrary splits. `common_rate_nad` is
/// the center price for base input and `NAD` for quote input.
#[derive(Clone, Copy)]
pub(crate) struct PreparedCommonDivergencePotential {
    balanced_common_nad: u128,
    start_asset_reserve_nad: u128,
    start_common_nad: u128,
    common_rate_nad: u64,
    input_decimals: u8,
    coefficient_nad: u64,
    /// Protocol constants folded once for every endpoint probe:
    /// `4 * coefficient_nad` in the numerator and
    /// `3 * common_rate_nad * decimal_scale` in the denominator.
    coefficient_times_four: u128,
    denominator_rate_scale: u128,
    start_raw_state_potential: U512,
}

fn canonical_common_coordinate(asset_reserve_nad: u128, common_rate_nad: u64) -> Result<u128> {
    require!(common_rate_nad > 0, ErrorCode::InvalidArgument);
    let value = U256::from(asset_reserve_nad)
        .checked_mul(U256::from(common_rate_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?
        / U256::from(NAD);
    require!(value <= U256::from(u128::MAX), ErrorCode::MarketMathOverflow);
    Ok(value.as_u128())
}

fn common_raw_state_potential_u256_prepared(
    outward_common_nad: u128,
    balanced_common_nad: u128,
    coefficient_times_four: u128,
    denominator_rate_scale: u128,
) -> Option<U512> {
    let outward = U256::from(outward_common_nad);
    let balanced = U256::from(balanced_common_nad);
    let numerator = outward
        .checked_mul(outward)?
        .checked_mul(outward)?
        .checked_mul(U256::from(coefficient_times_four))?;
    let denominator = balanced
        .checked_mul(balanced.checked_add(outward)?)?
        .checked_mul(U256::from(denominator_rate_scale))?;
    if denominator.is_zero() {
        return None;
    }
    let value = numerator / denominator;
    Some(U512([value.0[0], value.0[1], value.0[2], value.0[3], 0, 0, 0, 0]))
}

fn common_raw_state_potential_wide_prepared(
    common_coordinate_nad: u128,
    balanced_common_nad: u128,
    coefficient_times_four: u128,
    denominator_rate_scale: u128,
) -> Result<U512> {
    require!(
        balanced_common_nad > 0 && common_coordinate_nad <= MAX_COMMON_RESERVE,
        ErrorCode::InvalidArgument
    );
    let outward_common_nad = common_coordinate_nad.saturating_sub(balanced_common_nad);
    if outward_common_nad == 0 || coefficient_times_four == 0 {
        return Ok(U512::zero());
    }
    if let Some(value) = common_raw_state_potential_u256_prepared(
        outward_common_nad,
        balanced_common_nad,
        coefficient_times_four,
        denominator_rate_scale,
    ) {
        return Ok(value);
    }

    let outward = U512::from(outward_common_nad);
    let balanced = U512::from(balanced_common_nad);
    let numerator = outward
        .checked_mul(outward)
        .and_then(|value| value.checked_mul(outward))
        .and_then(|value| value.checked_mul(U512::from(coefficient_times_four)))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator = balanced
        .checked_mul(balanced.checked_add(outward).ok_or(ErrorCode::MarketMathOverflow)?)
        .and_then(|value| value.checked_mul(U512::from(denominator_rate_scale)))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(!denominator.is_zero(), ErrorCode::InvalidArgument);
    Ok(numerator / denominator)
}

#[cfg(test)]
fn common_raw_state_potential_u512_reference(
    common_coordinate_nad: u128,
    balanced_common_nad: u128,
    common_rate_nad: u64,
    decimal_scale: u128,
    coefficient_nad: u64,
) -> Result<U512> {
    require!(balanced_common_nad > 0, ErrorCode::InvalidArgument);
    let outward_common_nad = common_coordinate_nad.saturating_sub(balanced_common_nad);
    if outward_common_nad == 0 || coefficient_nad == 0 {
        return Ok(U512::zero());
    }
    let outward = U512::from(outward_common_nad);
    let balanced = U512::from(balanced_common_nad);
    let numerator = outward
        .checked_mul(outward)
        .and_then(|value| value.checked_mul(outward))
        .and_then(|value| value.checked_mul(U512::from(coefficient_nad)))
        .and_then(|value| value.checked_mul(U512::from(4_u8)))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator = balanced
        .checked_mul(balanced.checked_add(outward).ok_or(ErrorCode::MarketMathOverflow)?)
        .and_then(|value| value.checked_mul(U512::from(common_rate_nad)))
        .and_then(|value| value.checked_mul(U512::from(3_u8)))
        .and_then(|value| value.checked_mul(U512::from(decimal_scale)))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(!denominator.is_zero(), ErrorCode::InvalidArgument);
    Ok(numerator / denominator)
}

pub(crate) fn prepare_common_divergence_potential(
    balanced_common_nad: u128,
    start_asset_reserve_nad: u128,
    start_common_nad: u128,
    common_rate_nad: u64,
    input_decimals: u8,
    coefficient_nad: u64,
) -> Result<PreparedCommonDivergencePotential> {
    require!(
        balanced_common_nad > 0 && start_common_nad <= MAX_COMMON_RESERVE,
        ErrorCode::InvalidArgument
    );
    require!(input_decimals <= NAD_DECIMALS, ErrorCode::UnsupportedAssetDecimals);
    require_eq!(
        canonical_common_coordinate(start_asset_reserve_nad, common_rate_nad)?,
        start_common_nad,
        ErrorCode::BrokenInvariant
    );
    let decimal_scale = 10_u128
        .checked_pow((NAD_DECIMALS - input_decimals) as u32)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let coefficient_times_four = (coefficient_nad as u128)
        .checked_mul(4)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator_rate_scale = (common_rate_nad as u128)
        .checked_mul(3)
        .and_then(|value| value.checked_mul(decimal_scale))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let start_raw_state_potential = common_raw_state_potential_wide_prepared(
        start_common_nad,
        balanced_common_nad,
        coefficient_times_four,
        denominator_rate_scale,
    )?;
    Ok(PreparedCommonDivergencePotential {
        balanced_common_nad,
        start_asset_reserve_nad,
        start_common_nad,
        common_rate_nad,
        input_decimals,
        coefficient_nad,
        coefficient_times_four,
        denominator_rate_scale,
        start_raw_state_potential,
    })
}

impl PreparedCommonDivergencePotential {
    fn endpoint_common_nad(self, executable_input_raw: u64) -> Result<Option<u128>> {
        let input_nad = normalize_to_nad(executable_input_raw as u128, self.input_decimals)?;
        let end_asset_reserve_nad = self
            .start_asset_reserve_nad
            .checked_add(input_nad)
            .ok_or(ErrorCode::ReserveOverflow)?;
        let end_common_nad = canonical_common_coordinate(end_asset_reserve_nad, self.common_rate_nad)?;
        Ok((end_common_nad <= MAX_COMMON_RESERVE).then_some(end_common_nad))
    }

    pub(crate) fn fee_raw_saturating(self, executable_input_raw: u64) -> Result<(u128, bool)> {
        if executable_input_raw == 0 || self.coefficient_nad == 0 {
            return Ok((0, false));
        }
        let Some(end_common_nad) = self.endpoint_common_nad(executable_input_raw)? else {
            return Ok((u128::MAX, true));
        };
        require_gte!(end_common_nad, self.start_common_nad, ErrorCode::BrokenInvariant);
        let end_raw_state_potential = common_raw_state_potential_wide_prepared(
            end_common_nad,
            self.balanced_common_nad,
            self.coefficient_times_four,
            self.denominator_rate_scale,
        )?;
        let fee = end_raw_state_potential
            .checked_sub(self.start_raw_state_potential)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if fee > U512::from(u128::MAX) {
            Ok((u128::MAX, true))
        } else {
            Ok((fee.as_u128(), false))
        }
    }

    pub(crate) fn marginal_rate_nad(self, executable_input_raw: u64) -> Result<u64> {
        let Some(end_common_nad) = self.endpoint_common_nad(executable_input_raw)? else {
            return Ok(u64::MAX);
        };
        outward_divergence_marginal_rate_nad(self.balanced_common_nad, end_common_nad, self.coefficient_nad)
    }
}

#[cfg(test)]
pub(crate) fn outward_divergence_fee_raw_saturating(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    end_input_reserve_nad: u128,
    input_decimals: u8,
    coefficient_nad: u64,
) -> Result<(u128, bool)> {
    let prepared =
        prepare_outward_divergence_potential(center_input_reserve_nad, start_input_reserve_nad, coefficient_nad)?;
    outward_divergence_fee_raw_saturating_prepared(&prepared, end_input_reserve_nad, input_decimals)
}

fn denormalize_u512_ceil_saturating(amount_nad: U512, decimals: u8) -> Result<(u128, bool)> {
    let maximum = U512::from(u128::MAX);
    let value = match decimals.cmp(&NAD_DECIMALS) {
        std::cmp::Ordering::Equal => amount_nad,
        std::cmp::Ordering::Less => {
            let scale = U512::from(
                10_u128
                    .checked_pow((NAD_DECIMALS - decimals) as u32)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            );
            if amount_nad.is_zero() {
                U512::zero()
            } else {
                (amount_nad - U512::one()) / scale + U512::one()
            }
        }
        std::cmp::Ordering::Greater => {
            let scale = U512::from(
                10_u128
                    .checked_pow((decimals - NAD_DECIMALS) as u32)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            );
            if amount_nad > maximum / scale {
                return Ok((u128::MAX, true));
            }
            amount_nad.checked_mul(scale).ok_or(ErrorCode::MarketMathOverflow)?
        }
    };
    if value > maximum {
        Ok((u128::MAX, true))
    } else {
        Ok((value.as_u128(), false))
    }
}

/// NAD-scaled continuous marginal rate of the divergence potential at one
/// executable input-reserve coordinate.
///
/// This is the derivative of `divergence_state_potential_wide`. It is exposed
/// only inside the crate so the swap engine can solve the fee-adjusted endpoint
/// without duplicating the protocol formula. Values beyond `u64` are
/// deliberately saturated: the marginal is only a Newton accelerator, while
/// exact potential costs remain the authoritative feasibility proof.
pub(crate) fn outward_divergence_marginal_rate_nad(
    center_input_reserve_nad: u128,
    input_reserve_nad: u128,
    coefficient_nad: u64,
) -> Result<u64> {
    require!(center_input_reserve_nad > 0, ErrorCode::InvalidArgument);
    if coefficient_nad == 0 || input_reserve_nad <= center_input_reserve_nad {
        return Ok(0);
    }

    if let Some(rate) =
        outward_divergence_marginal_rate_u256(center_input_reserve_nad, input_reserve_nad, coefficient_nad)
    {
        return Ok(rate);
    }
    outward_divergence_marginal_rate_u512(center_input_reserve_nad, input_reserve_nad, coefficient_nad)
}

fn outward_divergence_marginal_rate_u256(
    center_input_reserve_nad: u128,
    input_reserve_nad: u128,
    coefficient_nad: u64,
) -> Option<u64> {
    let outward = U256::from(input_reserve_nad.checked_sub(center_input_reserve_nad)?);
    let center = U256::from(center_input_reserve_nad);
    let coefficient_times_four = U256::from(coefficient_nad).checked_mul(U256::from(4_u8))?;
    let outward_squared = outward.checked_mul(outward)?;
    let slope_factor = center
        .checked_mul(U256::from(3_u8))?
        .checked_add(outward.checked_mul(U256::from(2_u8))?)?;
    let numerator = outward_squared
        .checked_mul(coefficient_times_four)?
        .checked_mul(slope_factor)?;
    let center_plus_outward = center.checked_add(outward)?;
    let denominator = center
        .checked_mul(center_plus_outward)?
        .checked_mul(center_plus_outward)?
        .checked_mul(U256::from(3_u8))?;
    if denominator.is_zero() {
        return None;
    }
    let rate = numerator / denominator;
    Some(if rate > U256::from(u64::MAX) {
        u64::MAX
    } else {
        rate.as_u64()
    })
}

fn outward_divergence_marginal_rate_u512(
    center_input_reserve_nad: u128,
    input_reserve_nad: u128,
    coefficient_nad: u64,
) -> Result<u64> {
    let outward = U512::from(
        input_reserve_nad
            .checked_sub(center_input_reserve_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?,
    );
    let center = U512::from(center_input_reserve_nad);
    let coefficient_times_four = U512::from(coefficient_nad)
        .checked_mul(U512::from(4_u8))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let outward_squared = outward.checked_mul(outward).ok_or(ErrorCode::MarketMathOverflow)?;
    let slope_factor = center
        .checked_mul(U512::from(3_u8))
        .and_then(|value| value.checked_add(outward.checked_mul(U512::from(2_u8))?))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let numerator = outward_squared
        .checked_mul(coefficient_times_four)
        .and_then(|value| value.checked_mul(slope_factor))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let center_plus_outward = center.checked_add(outward).ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator = center
        .checked_mul(center_plus_outward)
        .and_then(|value| value.checked_mul(center_plus_outward))
        .and_then(|value| value.checked_mul(U512::from(3_u8)))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let rate = numerator / denominator;
    Ok(if rate > U512::from(u64::MAX) {
        u64::MAX
    } else {
        rate.as_u64()
    })
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
    exponential_price_decay(accumulator_nad, elapsed_ms, half_life_ms)
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
    let move_nad = symmetric_ratio_distance_nad(start_price_nad, end_price_nad)?;
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
pub(crate) fn quote_dynamic_fee(
    config: DynamicFeeConfig,
    pre_state: DynamicFeePreState,
    path: DynamicFeePath,
) -> Result<DynamicFeeQuote> {
    config.validate()?;
    require!(path.amount_in > 0, ErrorCode::AmountZero);

    let decayed_volatility_nad = decay_volatility_nad(
        pre_state
            .volatility_accumulator_nad
            .min(config.volatility_accumulator_cap_nad),
        pre_state.volatility_last_update_slot,
        path.current_slot,
        config.volatility_half_life_ms,
    )?;
    let base_fee_amount = fee_amount_ceil(path.amount_in, config.base_fee_rate_nad)?;
    let after_base = path
        .amount_in
        .checked_sub(base_fee_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(after_base > 0, ErrorCode::InsufficientOutputAmount);

    // Volatility pressure is naturally bounded below 100% and is applied only
    // to input left after the base fee. Floor rounding preserves at least one
    // atom for every finite rate.
    let volatility_marginal_rate_nad =
        asymptotic_scaled_rate_nad(decayed_volatility_nad as u128, config.volatility_coefficient_nad)?;
    let volatility_surcharge_amount = fee_amount_floor(after_base, volatility_marginal_rate_nad)?;
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
    require!(
        divergence_surcharge_amount < after_volatility,
        ErrorCode::InsufficientOutputAmount
    );
    let amount_in_for_quote = after_volatility
        .checked_sub(divergence_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    debug_assert!(amount_in_for_quote > 0);

    let dynamic_surcharge_amount = divergence_surcharge_amount
        .checked_add(volatility_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    let total_fee_amount = base_fee_amount
        .checked_add(dynamic_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(total_fee_amount < path.amount_in, ErrorCode::InsufficientOutputAmount);

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

fn effective_rate_floor_nad(amount: u64, total_amount: u64) -> Result<u64> {
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

fn fee_amount_floor(amount: u64, fee_rate_nad: u64) -> Result<u64> {
    require!(fee_rate_nad < NAD, ErrorCode::InvalidSwapFeeBps);
    let fee = (amount as u128)
        .checked_mul(fee_rate_nad as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?
        / NAD as u128;
    let fee = u64::try_from(fee).map_err(|_| ErrorCode::MarketMathOverflow)?;
    require!(fee < amount, ErrorCode::BrokenInvariant);
    Ok(fee)
}

/// Converts non-negative pressure `signal * coefficient` into the asymptotic
/// fee rate `pressure / (1 + pressure)`. It is linear near zero, monotonically
/// increasing, and strictly below 100% for every finite input. Production
/// signal/coefficient bounds limit pressure; this mapping is not the
/// unbounded divergence toll.
fn asymptotic_scaled_rate_nad(signal_nad: u128, coefficient_nad: u64) -> Result<u64> {
    if signal_nad == 0 || coefficient_nad == 0 {
        return Ok(0);
    }

    // If p = signal*coefficient/NAD^2, then rate = p/(1+p).
    // Keeping the unsimplified numerator in U512 avoids overflow and avoids an
    // early rounding step at low pressure.
    let pressure_numerator = U512::from(signal_nad)
        .checked_mul(U512::from(coefficient_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let nad = U512::from(NAD);
    let denominator = nad
        .checked_mul(nad)
        .and_then(|nad_squared| nad_squared.checked_add(pressure_numerator))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let rate = pressure_numerator
        .checked_mul(nad)
        .ok_or(ErrorCode::MarketMathOverflow)?
        / denominator;
    require!(rate < nad, ErrorCode::BrokenInvariant);
    u64::try_from(rate.as_u128()).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
mod tests {
    include!("../tests/math/dynamic_fee.rs");
}
