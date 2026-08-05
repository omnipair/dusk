use anchor_lang::prelude::*;
#[cfg(test)]
use std::cell::Cell;

use super::{CurveCheckpoint, CurveReservesNad, Market, MarketAsset};
use crate::{
    constants::{BPS_DENOMINATOR, NAD, NAD_DECIMALS},
    errors::ErrorCode,
    math::{
        decay_volatility_nad, divergence_state_potential_raw_saturating, effective_rate_floor_nad, normalize_to_nad,
        volatility_after_success_nad, ConcentratedCommonNumeraire, ConcentratedHybridBranch, ConcentratedInvariantSeed,
        ConcentratedSwapDirection, DynamicFeeConfig, DynamicFeePreState, DynamicFeeQuote, MAX_COMMON_RESERVE,
    },
    shared::math::ceil_div,
};

#[cfg(test)]
use crate::math::{
    outward_divergence_fee_raw_saturating_prepared, prepare_outward_divergence_potential,
    PreparedOutwardDivergencePotential,
};

/// The implicit divergence endpoint is a one-dimensional convex solve.
/// Secant and Newton probes accelerate the ordinary path, while the exact
/// feasible/infeasible cost bracket remains authoritative even when the
/// unbounded toll or its derivative saturates an accelerator. If token
/// granularity leaves no exact root, the lower endpoint deliberately charges
/// the rounding gap as fee.
const DIVERGENCE_ENDPOINT_MAX_ITERS: usize = u64::BITS as usize;

#[cfg(test)]
thread_local! {
    static DIVERGENCE_ENDPOINT_ITERATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_divergence_endpoint_iterations() {
    DIVERGENCE_ENDPOINT_ITERATIONS.with(|iterations| iterations.set(0));
}

#[cfg(test)]
fn divergence_endpoint_iterations() -> usize {
    DIVERGENCE_ENDPOINT_ITERATIONS.with(Cell::get)
}

#[derive(Clone, Copy)]
struct PreparedSwapDivergencePotential {
    center_input_reserve_raw: u128,
    start_input_reserve_raw: u128,
    coefficient_nad: u64,
}

impl PreparedSwapDivergencePotential {
    fn total_cost_probe(self, executable_input: u64) -> Result<(u128, bool)> {
        if executable_input == 0 {
            return Ok((0, false));
        }
        if self.coefficient_nad == 0 {
            return Ok((executable_input as u128, false));
        }
        let Ok(center_raw) = u64::try_from(self.center_input_reserve_raw) else {
            // A center above the complete u64 token-account domain means every
            // possible endpoint is still restorative; it is not a saturated
            // outward fee.
            return Ok((executable_input as u128, false));
        };
        let Ok(start_raw) = u64::try_from(self.start_input_reserve_raw) else {
            return Ok((u128::MAX, true));
        };
        let Some(end_raw) = start_raw.checked_add(executable_input) else {
            return Ok((u128::MAX, true));
        };
        let start_outward = start_raw.saturating_sub(center_raw);
        let end_outward = end_raw.saturating_sub(center_raw);
        let (fee, fee_saturated) = if end_outward <= start_outward {
            (0, false)
        } else {
            let (start, start_saturated) =
                divergence_state_potential_raw_saturating(start_outward, center_raw, self.coefficient_nad)?;
            let (end, end_saturated) =
                divergence_state_potential_raw_saturating(end_outward, center_raw, self.coefficient_nad)?;
            if start_saturated || end_saturated {
                (u128::MAX, true)
            } else {
                (end.checked_sub(start).ok_or(ErrorCode::MarketMathOverflow)?, false)
            }
        };
        if fee_saturated {
            return Ok((u128::MAX, true));
        }
        match (executable_input as u128).checked_add(fee) {
            Some(cost) => Ok((cost, false)),
            None => Ok((u128::MAX, true)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapFeeBreakdown {
    pub reserve_credit: u64,
    pub base_fee_debit: u64,
    pub divergence_surcharge_debit: u64,
    pub volatility_surcharge_debit: u64,
    pub dynamic_surcharge_debit: u64,
    pub total_fee_debit: u64,
    pub retained_surcharge: u64,
    pub distributed_surcharge_debit: u64,
    pub amount_in_for_quote: u64,
    pub reserve_input_credit: u64,
    pub claimable_fee_debit: u64,
    pub base_fee_rate_nad: u64,
    pub divergence_fee_rate_nad: u64,
    pub volatility_fee_rate_nad: u64,
    pub total_fee_rate_nad: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmmSwapQuote {
    pub asset_in: MarketAsset,
    pub amount_out: u64,
    pub start_price_nad: u64,
    /// Marginal price at the invariant-preserving trader endpoint. Retained
    /// surcharge is excluded because it is principal funding, not traded flow.
    pub end_price_nad: u64,
    /// Marginal price after retained surcharge, if any, has been added to the
    /// executable reserve. This is the state used by the next quote and risk.
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    pub fee: SwapFeeBreakdown,
    endpoints: Option<AmmSwapEndpoints>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AmmSwapEndpoints {
    trade: CurveCheckpoint,
    reserve: CurveCheckpoint,
}

impl AmmSwapQuote {
    pub(crate) fn trade_endpoint(&self) -> Result<CurveCheckpoint> {
        self.endpoints
            .map(|endpoints| endpoints.trade)
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }

    pub(crate) fn reserve_endpoint(&self) -> Result<CurveCheckpoint> {
        self.endpoints
            .map(|endpoints| endpoints.reserve)
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }

    /// Leverage receipts intentionally contain only ABI-visible quote fields.
    /// Reconstructed quotes are valid for reserve-overlay simulations, but may
    /// never enter an endpoint-reusing execution path.
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub(crate) const fn new_without_endpoints(
        asset_in: MarketAsset,
        amount_out: u64,
        start_price_nad: u64,
        end_price_nad: u64,
        reserve_end_price_nad: u64,
        decayed_volatility_nad: u64,
        post_success_volatility_nad: u64,
        fee: SwapFeeBreakdown,
    ) -> Self {
        Self {
            asset_in,
            amount_out,
            start_price_nad,
            end_price_nad,
            reserve_end_price_nad,
            decayed_volatility_nad,
            post_success_volatility_nad,
            fee,
            endpoints: None,
        }
    }
}

/// Conservative first-pass coordinates for hLP pre-positioning.
///
/// The quote coordinate excludes every fee. The reserve coordinate adds back
/// dynamic surcharge that will remain as AMM principal. Divergence is omitted
/// from the first pass, so both coordinates are at least as outward as the
/// final executable path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PreliminarySwapInputs {
    pub amount_in_for_quote: u64,
    pub reserve_input_credit: u64,
    fee: DynamicFeeQuote,
}

impl Market {
    /// Net input used by the hLP pre-solver. It includes base and already-known
    /// volatility fees, but intentionally omits divergence. Because the final
    /// divergence fee can only reduce input, the pre-solve endpoint is a
    /// conservative outward path for the second pass.
    #[cfg(test)]
    pub(crate) fn preliminary_swap_inputs(
        &self,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<PreliminarySwapInputs> {
        require!(reserve_credit > 0, ErrorCode::AmountZero);
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        self.preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
    }

    pub(crate) fn preliminary_swap_inputs_for_state(
        &self,
        reserve_credit: u64,
        current_slot: u64,
        pre_state: DynamicFeePreState,
    ) -> Result<PreliminarySwapInputs> {
        require!(reserve_credit > 0, ErrorCode::AmountZero);
        let config = self.dynamic_fee_config()?;
        require!(config.base_fee_rate_nad < NAD, ErrorCode::InvalidSwapFeeBps);
        require!(
            config.volatility_shock_cap_nad <= config.volatility_accumulator_cap_nad,
            ErrorCode::InvalidArgument
        );
        require!(
            config.volatility_coefficient_nad == 0 || config.volatility_half_life_ms > 0,
            ErrorCode::InvalidHalfLife
        );
        let decayed_volatility_nad = decay_volatility_nad(
            pre_state
                .volatility_accumulator_nad
                .min(config.volatility_accumulator_cap_nad),
            pre_state.volatility_last_update_slot,
            current_slot,
            config.volatility_half_life_ms,
        )?;
        let base_fee_amount = u64::try_from(
            ceil_div(
                (reserve_credit as u128)
                    .checked_mul(config.base_fee_rate_nad as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                NAD as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let after_base = reserve_credit
            .checked_sub(base_fee_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(after_base > 0, ErrorCode::InsufficientOutputAmount);

        let signal_nad = decayed_volatility_nad as u128;
        let coefficient_nad = config.volatility_coefficient_nad;
        let volatility_rate_nad = if signal_nad == 0 || coefficient_nad == 0 {
            0
        } else if let Some(pressure_numerator) = signal_nad.checked_mul(coefficient_nad as u128) {
            let nad_squared = (NAD as u128)
                .checked_mul(NAD as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            if let Some(denominator) = nad_squared.checked_add(pressure_numerator) {
                let nad_cubed = nad_squared
                    .checked_mul(NAD as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let complement = (nad_cubed - 1)
                    .checked_div(denominator)
                    .and_then(|value| value.checked_add(1))
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(
                    (NAD as u128)
                        .checked_sub(complement)
                        .ok_or(ErrorCode::BrokenInvariant)?,
                )
                .map_err(|_| ErrorCode::MarketMathOverflow)?
            } else {
                NAD - 1
            }
        } else {
            NAD - 1
        };
        require!(volatility_rate_nad < NAD, ErrorCode::InvalidSwapFeeBps);
        let volatility_surcharge_amount = u64::try_from(
            (after_base as u128)
                .checked_mul(volatility_rate_nad as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?
                / NAD as u128,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(volatility_surcharge_amount < after_base, ErrorCode::BrokenInvariant);
        let amount_in_for_quote = after_base
            .checked_sub(volatility_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(amount_in_for_quote > 0, ErrorCode::InsufficientOutputAmount);
        let total_fee_amount = base_fee_amount
            .checked_add(volatility_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(total_fee_amount < reserve_credit, ErrorCode::InsufficientOutputAmount);
        let base_rate_nad = effective_rate_floor_nad(base_fee_amount, reserve_credit)?;
        let volatility_rate_nad = effective_rate_floor_nad(volatility_surcharge_amount, reserve_credit)?;
        let total_rate_nad = base_rate_nad
            .checked_add(volatility_rate_nad)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(total_rate_nad < NAD, ErrorCode::BrokenInvariant);
        let preliminary = DynamicFeeQuote {
            base_rate_nad,
            divergence_rate_nad: 0,
            volatility_rate_nad,
            total_rate_nad,
            base_fee_amount,
            divergence_surcharge_amount: 0,
            volatility_surcharge_amount,
            dynamic_surcharge_amount: volatility_surcharge_amount,
            total_fee_amount,
            decayed_volatility_nad,
            post_success_volatility_nad: decayed_volatility_nad,
        };
        let amount = reserve_credit
            .checked_sub(preliminary.total_fee_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(amount > 0, ErrorCode::InsufficientOutputAmount);
        let reserve_input_credit = if self.amm.retain_dynamic_surcharge {
            // Every dynamic surcharge, including the divergence omitted from
            // this first pass, remains in the reserve. Therefore only the base
            // fee leaves the reserve coordinate.
            reserve_credit
                .checked_sub(preliminary.base_fee_amount)
                .ok_or(ErrorCode::FeeMathOverflow)?
        } else {
            // Without retention, omitted divergence can only lower the final
            // quote and reserve inputs, so the no-divergence amount is the
            // conservative outward endpoint.
            amount
        };
        require_gte!(reserve_input_credit, amount, ErrorCode::BrokenInvariant);
        Ok(PreliminarySwapInputs {
            amount_in_for_quote: amount,
            reserve_input_credit,
            fee: preliminary,
        })
    }

    /// Deterministic two-pass quote. The caller may run the hLP pre-solver with
    /// `preliminary_swap_input` first; this method then freezes the resulting
    /// curve state, obtains a conservative no-divergence endpoint, charges the
    /// path fee, and quotes once more with the final net input.
    #[cfg(test)]
    pub(crate) fn quote_amm_swap(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<AmmSwapQuote> {
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        let preliminary = self.preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)?;
        self.quote_amm_swap_for_reserves_nad(
            asset_in,
            reserve_credit,
            current_slot,
            self.curve_reserves_nad()?,
            pre_state,
            preliminary,
        )
    }

    /// Quotes a second trade against the executable reserves left by `first`
    /// without mutating EMA, protected-liquidity, or ramp state. This is used
    /// by leverage health checks to price the exact unwind that would follow a
    /// successful opening/increase/decrease trade.
    #[cfg(test)]
    pub(crate) fn quote_amm_swap_after(
        &self,
        first: &AmmSwapQuote,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<AmmSwapQuote> {
        require_eq!(
            first.fee.reserve_input_credit,
            first
                .fee
                .amount_in_for_quote
                .checked_add(first.fee.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        let mut reserves = self.curve_reserves_nad()?;
        let input_nad = normalize_to_nad(
            first.fee.reserve_input_credit as u128,
            self.side(first.asset_in).asset_decimals,
        )?;
        let output_nad = normalize_to_nad(
            first.amount_out as u128,
            self.side(first.asset_in.opposite()).asset_decimals,
        )?;
        match first.asset_in {
            MarketAsset::Base => {
                reserves.base = reserves.base.checked_add(input_nad).ok_or(ErrorCode::ReserveOverflow)?;
                reserves.quote = reserves
                    .quote
                    .checked_sub(output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
            }
            MarketAsset::Quote => {
                reserves.quote = reserves
                    .quote
                    .checked_add(input_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                reserves.base = reserves
                    .base
                    .checked_sub(output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
            }
        }
        let pre_state = DynamicFeePreState {
            center_price_nad: self.current_curve_center_price_nad()?,
            volatility_accumulator_nad: first.post_success_volatility_nad,
            volatility_last_update_slot: current_slot,
        };
        let preliminary = self.preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)?;
        self.quote_amm_swap_for_reserves_nad(asset_in, reserve_credit, current_slot, reserves, pre_state, preliminary)
    }

    pub(crate) fn quote_amm_swap_for_reserves_nad(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
        reserves: CurveReservesNad,
        pre_state: DynamicFeePreState,
        preliminary: PreliminarySwapInputs,
    ) -> Result<AmmSwapQuote> {
        // Preliminary fee input depends only on the frozen accumulator. The
        // invariant-coordinate divergence potential needs starting D and the
        // input-reserve displacement, not a provisional output quote or its
        // marginal prices. Avoiding that redundant CONCENTRATED quote removes an entire
        // reserve solve plus two marginal-price proofs from every swap.
        let preliminary_input = preliminary.amount_in_for_quote;
        let prepared = self.prepare_curve_for_reserves_nad(reserves, pre_state.center_price_nad, current_slot)?;
        let direction = match asset_in {
            MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
            MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
        };
        if self.amm.retain_dynamic_surcharge && prepared.peak_depth_nad != 0 {
            // Retained dynamic fees are deposited after the invariant-preserving
            // exchange. Their maximum reserve credit is already known before
            // the implicit divergence solve. Reject an endpoint that must leave
            // the bounded Q48 common-coordinate domain instead of spending the
            // full solver budget on a quote that cannot be committed.
            let input_decimals = self.side(asset_in).asset_decimals;
            let retained_endpoint_input_nad = match asset_in {
                MarketAsset::Base => reserves
                    .base
                    .checked_add(normalize_to_nad(
                        preliminary.reserve_input_credit as u128,
                        input_decimals,
                    )?)
                    .ok_or(ErrorCode::ReserveOverflow)?,
                MarketAsset::Quote => reserves
                    .quote
                    .checked_add(normalize_to_nad(
                        preliminary.reserve_input_credit as u128,
                        input_decimals,
                    )?)
                    .ok_or(ErrorCode::ReserveOverflow)?,
            };
            let retained_endpoint_common_nad = prepared
                .input_common_scale(direction)?
                .to_common_floor(retained_endpoint_input_nad)?;
            require!(
                retained_endpoint_common_nad <= MAX_COMMON_RESERVE,
                ErrorCode::InvalidSettlementPrice
            );
        }
        let config = self.dynamic_fee_config()?;
        let invariant_d_nad = prepared.invariant_d();
        let start_input_reserve_nad = match asset_in {
            MarketAsset::Base => reserves.base,
            MarketAsset::Quote => reserves.quote,
        };
        // Solve against the executable endpoint, rather than the hypothetical
        // no-divergence endpoint. If `z` is the post-base/post-volatility
        // input and `d` is executable input, this enforces
        //
        //   d + F(x + d) - F(x) = z.
        //
        // For distributed surcharge, consecutive outward trades therefore
        // telescope through the same state potential instead of receiving a
        // split discount. Retained surcharge is deposited only after the
        // invariant-preserving trade, so it is correctly excluded from the
        // charged exchange path. The resulting next-state D behavior is
        // exercised separately by the retained split matrix.
        let input_decimals = self.side(asset_in).asset_decimals;
        require!(input_decimals <= NAD_DECIMALS, ErrorCode::UnsupportedAssetDecimals);
        let divergence_surcharge_amount = if config.divergence_coefficient_nad == 0 {
            0
        } else {
            require!(invariant_d_nad > 0, ErrorCode::BrokenInvariant);
            let decimal_scale = 10_u128
                .checked_pow((NAD_DECIMALS - input_decimals) as u32)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            // Compute the balanced threshold directly in raw input atoms. A
            // NAD-floor followed by a raw ceil can move the threshold one atom
            // inward and charge restorative flow. The common scaling cancels
            // algebraically from F, so CPMM and concentrated pools share this
            // one physical input-reserve potential.
            let (center_raw_numerator, center_raw_denominator) = match (prepared.common_numeraire(), asset_in) {
                (ConcentratedCommonNumeraire::Quote, MarketAsset::Base) => (
                    invariant_d_nad
                        .checked_mul(NAD as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    (pre_state.center_price_nad as u128)
                        .checked_mul(2)
                        .and_then(|value| value.checked_mul(decimal_scale))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                ),
                (ConcentratedCommonNumeraire::Quote, MarketAsset::Quote)
                | (ConcentratedCommonNumeraire::Base, MarketAsset::Base) => (
                    invariant_d_nad,
                    decimal_scale.checked_mul(2).ok_or(ErrorCode::MarketMathOverflow)?,
                ),
                (ConcentratedCommonNumeraire::Base, MarketAsset::Quote) => (
                    invariant_d_nad
                        .checked_mul(pre_state.center_price_nad as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    (NAD as u128)
                        .checked_mul(2)
                        .and_then(|value| value.checked_mul(decimal_scale))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                ),
            };
            let center_input_reserve_raw =
                ceil_div(center_raw_numerator, center_raw_denominator).ok_or(ErrorCode::MarketMathOverflow)?;
            require!(center_input_reserve_raw > 0, ErrorCode::BrokenInvariant);
            require_eq!(start_input_reserve_nad % decimal_scale, 0, ErrorCode::BrokenInvariant);
            let potential = PreparedSwapDivergencePotential {
                center_input_reserve_raw,
                start_input_reserve_raw: start_input_reserve_nad / decimal_scale,
                coefficient_nad: config.divergence_coefficient_nad,
            };
            // Solve the swap endpoint beside fee composition so the exact
            // debit and rounding contract remains visible at its production
            // boundary. The test-only harness covers the raw-domain edges.
            'implicit_divergence: {
                let available = preliminary_input;
                require!(available > 0, ErrorCode::InsufficientOutputAmount);

                // Zero executable input is always feasible. Gross input is
                // either exactly fee-free or infeasible. Because the toll may
                // exceed gross, do not manufacture a lower bound by subtracting
                // from `available`.
                let mut low = 0_u64;
                let mut low_cost = 0_u128;
                let mut high = available;
                let (mut high_cost, mut high_cost_saturated) = potential.total_cost_probe(available)?;
                if !high_cost_saturated && high_cost == available as u128 {
                    break 'implicit_divergence 0;
                }
                require!(
                    high_cost_saturated || high_cost > available as u128,
                    ErrorCode::BrokenInvariant
                );

                // A saturated gross probe can otherwise require a full
                // 64-round midpoint walk merely to discover that even the
                // minimum curve-executable input quantum is unaffordable.
                if high_cost_saturated && high > 1 {
                    let (one_cost, one_cost_saturated) = potential.total_cost_probe(1)?;
                    if one_cost_saturated || one_cost > available as u128 {
                        break 'implicit_divergence available;
                    }
                    low = 1;
                    low_cost = one_cost;
                }

                // The first probe reuses the endpoint costs. Subsequent
                // probes are safeguarded Newton steps; midpoint fallback
                // remains authoritative whenever rounding or saturation
                // makes the accelerator unsuitable.
                let mut first_probe = true;
                let mut probe_from_high = true;
                for iteration in 0..DIVERGENCE_ENDPOINT_MAX_ITERS {
                    if low_cost == available as u128 || high - low <= 1 {
                        break;
                    }
                    #[cfg(test)]
                    DIVERGENCE_ENDPOINT_ITERATIONS.with(|iterations| iterations.set(iterations.get() + 1));

                    let mut probe = if first_probe && !high_cost_saturated {
                        first_probe = false;
                        let cost_span = high_cost.checked_sub(low_cost).ok_or(ErrorCode::FeeMathOverflow)?;
                        let target_offset = (available as u128)
                            .checked_sub(low_cost)
                            .ok_or(ErrorCode::FeeMathOverflow)?;
                        let reserve_span = (high - low) as u128;
                        let interpolated_offset = target_offset
                            .checked_mul(reserve_span)
                            .and_then(|value| value.checked_div(cost_span))
                            .ok_or(ErrorCode::FeeMathOverflow)?;
                        low.checked_add(u64::try_from(interpolated_offset).map_err(|_| ErrorCode::FeeMathOverflow)?)
                            .ok_or(ErrorCode::FeeMathOverflow)?
                    } else if first_probe {
                        first_probe = false;
                        low + (high - low) / 2
                    } else {
                        let (origin, residual, add_probe) = if probe_from_high {
                            if high_cost_saturated {
                                (0, 0, false)
                            } else {
                                (
                                    high,
                                    high_cost
                                        .checked_sub(available as u128)
                                        .ok_or(ErrorCode::FeeMathOverflow)?,
                                    false,
                                )
                            }
                        } else {
                            (
                                low,
                                (available as u128)
                                    .checked_sub(low_cost)
                                    .ok_or(ErrorCode::FeeMathOverflow)?,
                                true,
                            )
                        };
                        if origin == 0 && high_cost_saturated {
                            low + (high - low) / 2
                        } else {
                            let center_raw = u64::try_from(potential.center_input_reserve_raw)
                                .map_err(|_| ErrorCode::MarketMathOverflow)?;
                            let start_raw = u64::try_from(potential.start_input_reserve_raw)
                                .map_err(|_| ErrorCode::MarketMathOverflow)?;
                            let origin_reserve_raw = start_raw.checked_add(origin).ok_or(ErrorCode::ReserveOverflow)?;
                            let outward = origin_reserve_raw.saturating_sub(center_raw) as u128;
                            let marginal_rate_nad = if outward == 0 {
                                0
                            } else {
                                // dF/du = (4*k/3) * t^2 * (3-t)/(1-t),
                                // where t=u/(q0+u). Coordinates are already
                                // raw token atoms, so this accelerator needs
                                // no Euclidean normalization or wide product.
                                let endpoint = (center_raw as u128)
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
                                let slope_factor_q48 = q48
                                    .checked_mul(3)
                                    .and_then(|value| value.checked_sub(outward_fraction_q48))
                                    .ok_or(ErrorCode::MarketMathOverflow)?;
                                let shape_q48 = outward_squared_q48
                                    .checked_mul(slope_factor_q48)
                                    .and_then(|value| value.checked_div(center_fraction_q48))
                                    .ok_or(ErrorCode::MarketMathOverflow)?;
                                let coefficient_times_four = (potential.coefficient_nad as u128)
                                    .checked_mul(4)
                                    .ok_or(ErrorCode::MarketMathOverflow)?;
                                match shape_q48.checked_mul(coefficient_times_four) {
                                    Some(numerator) => {
                                        let rate = numerator
                                            .checked_div(q48.checked_mul(3).ok_or(ErrorCode::MarketMathOverflow)?)
                                            .ok_or(ErrorCode::MarketMathOverflow)?;
                                        rate.min(u64::MAX as u128) as u64
                                    }
                                    // Overflow implies a post-Q48 quotient
                                    // wider than u64; saturation preserves the
                                    // safeguarded Newton direction.
                                    None => u64::MAX,
                                }
                            };
                            let derivative_nad = (NAD as u128)
                                .checked_add(marginal_rate_nad as u128)
                                .ok_or(ErrorCode::FeeMathOverflow)?;
                            require_gte!(derivative_nad, NAD as u128, ErrorCode::BrokenInvariant);
                            let whole = residual
                                .checked_div(derivative_nad)
                                .and_then(|value| value.checked_mul(NAD as u128))
                                .ok_or(ErrorCode::FeeMathOverflow)?;
                            let remainder = residual.checked_rem(derivative_nad).ok_or(ErrorCode::FeeMathOverflow)?;
                            let remainder_numerator =
                                remainder.checked_mul(NAD as u128).ok_or(ErrorCode::FeeMathOverflow)?;
                            let fractional = if remainder_numerator == 0 {
                                0
                            } else {
                                (remainder_numerator - 1)
                                    .checked_div(derivative_nad)
                                    .and_then(|value| value.checked_add(1))
                                    .ok_or(ErrorCode::FeeMathOverflow)?
                            };
                            let step = u64::try_from(whole.checked_add(fractional).ok_or(ErrorCode::FeeMathOverflow)?)
                                .unwrap_or(u64::MAX)
                                .max(1);
                            if add_probe {
                                origin.checked_add(step).ok_or(ErrorCode::FeeMathOverflow)?
                            } else {
                                origin.saturating_sub(step)
                            }
                        }
                    };
                    if probe <= low || probe >= high {
                        probe = low + (high - low) / 2;
                    }

                    // Preserve hard liveness independently of accelerator
                    // quality: either possible child bracket must fit the
                    // ordinary bisections remaining after this round.
                    let remaining_rounds = DIVERGENCE_ENDPOINT_MAX_ITERS - iteration - 1;
                    let maximum_next_width = 1_u128.checked_shl(remaining_rounds as u32).unwrap_or(u128::MAX);
                    let minimum_safe_probe = (high as u128).saturating_sub(maximum_next_width).max((low as u128) + 1);
                    let maximum_safe_probe = (low as u128).saturating_add(maximum_next_width).min((high as u128) - 1);
                    require!(minimum_safe_probe <= maximum_safe_probe, ErrorCode::BrokenInvariant);
                    probe = u64::try_from((probe as u128).clamp(minimum_safe_probe, maximum_safe_probe))
                        .map_err(|_| ErrorCode::FeeMathOverflow)?;

                    let (probe_cost, probe_cost_saturated) = potential.total_cost_probe(probe)?;
                    if !probe_cost_saturated && probe_cost <= available as u128 {
                        low = probe;
                        low_cost = probe_cost;
                        probe_from_high = false;

                        // fee(low)=low_cost-low and
                        // deficit=available-low_cost. Because fee is
                        // nondecreasing, low+deficit is either the exact root
                        // or a tighter infeasible endpoint.
                        let deficit = (available as u128)
                            .checked_sub(low_cost)
                            .ok_or(ErrorCode::FeeMathOverflow)?;
                        let candidate = (low as u128).checked_add(deficit).ok_or(ErrorCode::FeeMathOverflow)?;
                        if candidate > low as u128 && candidate < high as u128 {
                            let candidate = u64::try_from(candidate).map_err(|_| ErrorCode::FeeMathOverflow)?;
                            let (candidate_cost, candidate_cost_saturated) = potential.total_cost_probe(candidate)?;
                            if candidate_cost_saturated || candidate_cost > available as u128 {
                                high = candidate;
                                high_cost = candidate_cost;
                                high_cost_saturated = candidate_cost_saturated;
                            } else {
                                require_eq!(candidate_cost, available as u128, ErrorCode::BrokenInvariant);
                                low = candidate;
                                low_cost = candidate_cost;
                            }
                        }
                    } else {
                        high = probe;
                        high_cost = probe_cost;
                        high_cost_saturated = probe_cost_saturated;
                        probe_from_high = true;
                    }
                }

                // Never accept an iteration-limit approximation. The exact
                // cost or adjacent infeasible endpoint proves maximality.
                require!(
                    low_cost == available as u128 || high - low <= 1,
                    ErrorCode::FeeMathOverflow
                );
                let surcharge = available.checked_sub(low).ok_or(ErrorCode::FeeMathOverflow)?;
                require!(surcharge > 0, ErrorCode::BrokenInvariant);
                break 'implicit_divergence surcharge;
            }
        };
        // Base fee, volatility decay, and the volatility surcharge were
        // frozen once in `preliminary`. Compose only the path-dependent
        // divergence debit here; rerunning the full fee quote would duplicate
        // its most expensive state-independent work (and did so a third time
        // when hLP predictive positioning was active).
        let mut dynamic = preliminary.fee;
        require_eq!(dynamic.divergence_surcharge_amount, 0, ErrorCode::BrokenInvariant);
        require!(
            divergence_surcharge_amount < preliminary.amount_in_for_quote,
            ErrorCode::InsufficientOutputAmount
        );
        dynamic.divergence_surcharge_amount = divergence_surcharge_amount;
        dynamic.dynamic_surcharge_amount = dynamic
            .volatility_surcharge_amount
            .checked_add(divergence_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        dynamic.total_fee_amount = dynamic
            .base_fee_amount
            .checked_add(dynamic.dynamic_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        dynamic.divergence_rate_nad = u64::try_from(
            (divergence_surcharge_amount as u128)
                .checked_mul(NAD as u128)
                .and_then(|value| value.checked_div(reserve_credit as u128))
                .ok_or(ErrorCode::FeeMathOverflow)?,
        )
        .map_err(|_| ErrorCode::FeeMathOverflow)?;
        dynamic.total_rate_nad = dynamic
            .base_rate_nad
            .checked_add(dynamic.volatility_rate_nad)
            .and_then(|value| value.checked_add(dynamic.divergence_rate_nad))
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(dynamic.total_rate_nad < NAD, ErrorCode::BrokenInvariant);
        let amount_in_for_quote = preliminary
            .amount_in_for_quote
            .checked_sub(divergence_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(amount_in_for_quote > 0, ErrorCode::InsufficientOutputAmount);
        let final_curve =
            self.quote_curve_exact_in_for_prepared_nad(asset_in, amount_in_for_quote, prepared, current_slot)?;

        let divergence_surcharge_debit = dynamic.divergence_surcharge_amount;
        let volatility_surcharge_debit = dynamic.volatility_surcharge_amount;
        let (retained_surcharge, distributed_surcharge_debit) = if self.amm.retain_dynamic_surcharge {
            (dynamic.dynamic_surcharge_amount, 0)
        } else {
            (0, dynamic.dynamic_surcharge_amount)
        };
        let claimable_fee_debit = dynamic
            .base_fee_amount
            .checked_add(distributed_surcharge_debit)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let reserve_input_credit = amount_in_for_quote
            .checked_add(retained_surcharge)
            .ok_or(ErrorCode::ReserveOverflow)?;
        require_eq!(
            reserve_input_credit
                .checked_add(claimable_fee_debit)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            reserve_credit,
            ErrorCode::BrokenInvariant
        );
        let trade_endpoint = final_curve.endpoint;
        let reserve_endpoint = if retained_surcharge == 0 {
            trade_endpoint
        } else {
            let mut endpoint_reserves = trade_endpoint.reserves;
            let retained_nad = normalize_to_nad(retained_surcharge as u128, self.side(asset_in).asset_decimals)?;
            require!(retained_nad > 0, ErrorCode::BrokenInvariant);
            match asset_in {
                MarketAsset::Base => {
                    endpoint_reserves.base = endpoint_reserves
                        .base
                        .checked_add(retained_nad)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                }
                MarketAsset::Quote => {
                    endpoint_reserves.quote = endpoint_reserves
                        .quote
                        .checked_add(retained_nad)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                }
            }
            let center_price_nad = prepared.center_price_nad;
            let numeraire = prepared.common_numeraire();
            let endpoint_base_common = numeraire
                .base_scale(center_price_nad)?
                .to_common_floor(endpoint_reserves.base)?;
            let endpoint_quote_common = numeraire
                .quote_scale(center_price_nad)?
                .to_common_floor(endpoint_reserves.quote)?;
            let endpoint_branch = if prepared.peak_depth_nad == 0 {
                require!(
                    endpoint_reserves.base > 0 && endpoint_reserves.quote > 0,
                    ErrorCode::InvalidArgument
                );
                ConcentratedHybridBranch::Inner
            } else {
                prepared
                    .geometry
                    .ok_or(ErrorCode::BrokenInvariant)?
                    .branch(endpoint_base_common, endpoint_quote_common)?
            };
            if endpoint_branch.is_exact_tail() {
                // An exact outer tail is CPMM. Prove its marginal mark
                // directly before paying for a second invariant solve. This
                // makes an extreme retained endpoint fail deterministically
                // instead of exhausting the SBF meter on a mark that must
                // round to zero.
                let tail_price_numerator = endpoint_quote_common
                    .checked_mul(center_price_nad)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                require!(
                    tail_price_numerator >= endpoint_base_common,
                    ErrorCode::InvalidSettlementPrice
                );
            }
            let prepared_endpoint = prepared.prepare_successor(
                endpoint_reserves.base,
                endpoint_reserves.quote,
                ConcentratedInvariantSeed::Hint(trade_endpoint.evaluation().invariant_d),
            )?;
            self.checkpoint_for_prepared_curve(prepared_endpoint, current_slot)?
        };
        let post_success_volatility_nad = volatility_after_success_nad(
            dynamic.decayed_volatility_nad,
            final_curve.start_price_nad,
            final_curve.end_price_nad,
            self.config.amm.volatility_shock_cap_nad,
            self.config.amm.volatility_cap_nad,
        )?;
        let reserve_end_price_nad = u64::try_from(reserve_endpoint.evaluation().marginal_price_nad)
            .map_err(|_| ErrorCode::MarketMathOverflow)?;
        // A canonical marginal mark below one NAD atom rounds to zero. Such a
        // quote cannot be consumed by the shared risk engine (or inverted for
        // the opposite-side mark), so reject it here before preview and
        // execution can disagree. Retention can make the reserve endpoint
        // materially farther out than the trader's invariant endpoint; both
        // marks therefore need the same fail-closed domain check.
        require!(
            final_curve.end_price_nad > 0 && reserve_end_price_nad > 0,
            ErrorCode::InvalidSettlementPrice
        );

        Ok(AmmSwapQuote {
            asset_in,
            amount_out: final_curve.amount_out,
            start_price_nad: final_curve.start_price_nad,
            end_price_nad: final_curve.end_price_nad,
            reserve_end_price_nad,
            decayed_volatility_nad: dynamic.decayed_volatility_nad,
            post_success_volatility_nad,
            fee: SwapFeeBreakdown {
                reserve_credit,
                base_fee_debit: dynamic.base_fee_amount,
                divergence_surcharge_debit,
                volatility_surcharge_debit,
                dynamic_surcharge_debit: dynamic.dynamic_surcharge_amount,
                total_fee_debit: dynamic.total_fee_amount,
                retained_surcharge,
                distributed_surcharge_debit,
                amount_in_for_quote,
                reserve_input_credit,
                claimable_fee_debit,
                base_fee_rate_nad: dynamic.base_rate_nad,
                divergence_fee_rate_nad: dynamic.divergence_rate_nad,
                volatility_fee_rate_nad: dynamic.volatility_rate_nad,
                total_fee_rate_nad: dynamic.total_rate_nad,
            },
            endpoints: Some(AmmSwapEndpoints {
                trade: trade_endpoint,
                reserve: reserve_endpoint,
            }),
        })
    }

    fn dynamic_fee_config(&self) -> Result<DynamicFeeConfig> {
        let base_fee_rate_nad = u64::try_from(
            (self.config.swap_fee_bps as u128)
                .checked_mul(NAD as u128)
                .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                .ok_or(ErrorCode::FeeMathOverflow)?,
        )
        .map_err(|_| ErrorCode::FeeMathOverflow)?;
        Ok(DynamicFeeConfig {
            base_fee_rate_nad,
            divergence_coefficient_nad: self.config.amm.divergence_fee_coefficient_nad,
            volatility_coefficient_nad: self.config.amm.volatility_fee_coefficient_nad,
            volatility_half_life_ms: self.config.amm.volatility_half_life_ms,
            volatility_shock_cap_nad: self.config.amm.volatility_shock_cap_nad,
            volatility_accumulator_cap_nad: self.config.amm.volatility_cap_nad,
        })
    }

    pub(crate) fn dynamic_fee_pre_state(&self, current_slot: u64) -> Result<DynamicFeePreState> {
        Ok(DynamicFeePreState {
            center_price_nad: self.current_curve_center_price_nad()?,
            volatility_accumulator_nad: if self.amm.initialized {
                self.amm.volatility_accumulator_nad
            } else {
                0
            },
            volatility_last_update_slot: if self.amm.initialized {
                self.amm.last_observation_slot
            } else {
                current_slot
            },
        })
    }
}

/// Finds the conservative raw-token solution of
///
/// `executable + divergence_potential(executable) = available`.
///
/// `low` always has total cost at most `available`; `high` always has total
/// cost above it. The continuous potential is convex and has an unbounded
/// marginal rate; raw-token rounding can make its discrete approximation
/// locally uneven. Every secant or Newton probe is therefore checked against
/// the exact-cost bracket, and an ordinary midpoint is used whenever rounding
/// or wide-value saturation would fail to shrink it.
#[cfg(test)]
fn implicit_divergence_surcharge_amount_core(
    divergence_potential: PreparedSwapDivergencePotential,
    available: u64,
) -> Result<u64> {
    require!(available > 0, ErrorCode::InsufficientOutputAmount);

    // Zero executable input is always feasible. Gross input is either exactly
    // fee-free or infeasible. Because the toll may exceed gross, do not
    // manufacture a lower bound by subtracting from `available`.
    let mut low = 0_u64;
    let mut low_cost = 0_u128;
    let mut high = available;
    let (mut high_cost, mut high_cost_saturated) = divergence_potential.total_cost_probe(available)?;
    if !high_cost_saturated && high_cost == available as u128 {
        return Ok(0);
    }
    require!(
        high_cost_saturated || high_cost > available as u128,
        ErrorCode::BrokenInvariant
    );

    // A saturated gross probe can otherwise require a full 64-round midpoint
    // walk merely to discover that even the minimum curve-executable input
    // quantum is unaffordable. Prove that boundary once. If it is feasible it
    // becomes a stronger lower endpoint; if not, returning the full residual
    // makes the quote reject at the positive curve-input gate.
    if high_cost_saturated && high > 1 {
        let (one_cost, one_cost_saturated) = divergence_potential.total_cost_probe(1)?;
        if one_cost_saturated || one_cost > available as u128 {
            return Ok(available);
        }
        low = 1;
        low_cost = one_cost;
    }

    // The first probe uses the exact endpoint costs already paid for above.
    // Linear interpolation avoids a marginal-rate evaluation on the ordinary
    // finite-cost path. If an endpoint cost is wider than u128, midpoint
    // fallback shrinks the raw-token bracket without interpreting saturation
    // as an economic value. Subsequent probes remain safeguarded Newton steps.
    let mut first_probe = true;
    let mut probe_from_high = true;
    for iteration in 0..DIVERGENCE_ENDPOINT_MAX_ITERS {
        if low_cost == available as u128 || high - low <= 1 {
            break;
        }
        #[cfg(test)]
        DIVERGENCE_ENDPOINT_ITERATIONS.with(|iterations| iterations.set(iterations.get() + 1));

        let mut probe = if first_probe && !high_cost_saturated {
            first_probe = false;
            let cost_span = high_cost.checked_sub(low_cost).ok_or(ErrorCode::FeeMathOverflow)?;
            let target_offset = (available as u128)
                .checked_sub(low_cost)
                .ok_or(ErrorCode::FeeMathOverflow)?;
            let reserve_span = (high - low) as u128;
            let interpolated_offset = target_offset
                .checked_mul(reserve_span)
                .and_then(|value| value.checked_div(cost_span))
                .ok_or(ErrorCode::FeeMathOverflow)?;
            low.checked_add(u64::try_from(interpolated_offset).map_err(|_| ErrorCode::FeeMathOverflow)?)
                .ok_or(ErrorCode::FeeMathOverflow)?
        } else if first_probe {
            first_probe = false;
            low + (high - low) / 2
        } else {
            let (origin, residual, add_probe) = if probe_from_high {
                if high_cost_saturated {
                    (0, 0, false)
                } else {
                    (
                        high,
                        high_cost
                            .checked_sub(available as u128)
                            .ok_or(ErrorCode::FeeMathOverflow)?,
                        false,
                    )
                }
            } else {
                (
                    low,
                    (available as u128)
                        .checked_sub(low_cost)
                        .ok_or(ErrorCode::FeeMathOverflow)?,
                    true,
                )
            };
            if origin == 0 && high_cost_saturated {
                low + (high - low) / 2
            } else {
                let center_raw = u64::try_from(divergence_potential.center_input_reserve_raw)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?;
                let start_raw = u64::try_from(divergence_potential.start_input_reserve_raw)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?;
                let origin_reserve_raw = start_raw.checked_add(origin).ok_or(ErrorCode::ReserveOverflow)?;
                let outward = origin_reserve_raw.saturating_sub(center_raw) as u128;
                let marginal_rate_nad = if outward == 0 {
                    0
                } else {
                    // dF/du = (4*k/3) * t^2 * (3-t)/(1-t), where
                    // t=u/(q0+u). All coordinates are already raw u64 token
                    // atoms, so no Euclidean normalization or wide-product
                    // fallback belongs in this Newton-only accelerator.
                    let endpoint = (center_raw as u128)
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
                    let slope_factor_q48 = q48
                        .checked_mul(3)
                        .and_then(|value| value.checked_sub(outward_fraction_q48))
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    let shape_q48 = outward_squared_q48
                        .checked_mul(slope_factor_q48)
                        .and_then(|value| value.checked_div(center_fraction_q48))
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    let coefficient_times_four = (divergence_potential.coefficient_nad as u128)
                        .checked_mul(4)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    match shape_q48.checked_mul(coefficient_times_four) {
                        Some(numerator) => {
                            let rate = numerator
                                .checked_div(q48.checked_mul(3).ok_or(ErrorCode::MarketMathOverflow)?)
                                .ok_or(ErrorCode::MarketMathOverflow)?;
                            rate.min(u64::MAX as u128) as u64
                        }
                        // Overflow here implies a post-Q48 quotient wider
                        // than u64, so saturation preserves the Newton step.
                        None => u64::MAX,
                    }
                };
                let derivative_nad = (NAD as u128)
                    .checked_add(marginal_rate_nad as u128)
                    .ok_or(ErrorCode::FeeMathOverflow)?;
                require_gte!(derivative_nad, NAD as u128, ErrorCode::BrokenInvariant);
                let whole = residual
                    .checked_div(derivative_nad)
                    .and_then(|value| value.checked_mul(NAD as u128))
                    .ok_or(ErrorCode::FeeMathOverflow)?;
                let remainder = residual.checked_rem(derivative_nad).ok_or(ErrorCode::FeeMathOverflow)?;
                let remainder_numerator = remainder.checked_mul(NAD as u128).ok_or(ErrorCode::FeeMathOverflow)?;
                let fractional = if remainder_numerator == 0 {
                    0
                } else {
                    (remainder_numerator - 1)
                        .checked_div(derivative_nad)
                        .and_then(|value| value.checked_add(1))
                        .ok_or(ErrorCode::FeeMathOverflow)?
                };
                let step = u64::try_from(whole.checked_add(fractional).ok_or(ErrorCode::FeeMathOverflow)?)
                    .unwrap_or(u64::MAX)
                    .max(1);
                if add_probe {
                    origin.checked_add(step).ok_or(ErrorCode::FeeMathOverflow)?
                } else {
                    origin.saturating_sub(step)
                }
            }
        };
        if probe <= low || probe >= high {
            probe = low + (high - low) / 2;
        }

        // Preserve a hard liveness proof independently of how accurate the
        // secant/Newton accelerator is. After this round, either possible
        // child bracket must fit the number of ordinary bisections remaining.
        // Slack earned by an earlier strong cut remains available to later
        // accelerator probes.
        let remaining_rounds = DIVERGENCE_ENDPOINT_MAX_ITERS - iteration - 1;
        let maximum_next_width = 1_u128.checked_shl(remaining_rounds as u32).unwrap_or(u128::MAX);
        let minimum_safe_probe = (high as u128).saturating_sub(maximum_next_width).max((low as u128) + 1);
        let maximum_safe_probe = (low as u128).saturating_add(maximum_next_width).min((high as u128) - 1);
        require!(minimum_safe_probe <= maximum_safe_probe, ErrorCode::BrokenInvariant);
        probe = u64::try_from((probe as u128).clamp(minimum_safe_probe, maximum_safe_probe))
            .map_err(|_| ErrorCode::FeeMathOverflow)?;

        let (probe_cost, probe_cost_saturated) = divergence_potential.total_cost_probe(probe)?;
        if !probe_cost_saturated && probe_cost <= available as u128 {
            low = probe;
            low_cost = probe_cost;
            // The fresh feasible endpoint is closest to the root in ordinary
            // cases, so start the next safeguarded Newton probe from it.
            probe_from_high = false;

            // Let fee(low) = low_cost-low and deficit = available-low_cost.
            // Because fee is nondecreasing, candidate=low+deficit has cost at
            // least `available`. Exact classification either finds the root
            // immediately or gives a tighter infeasible high endpoint. This
            // is especially effective when raw-token rounding leaves a long
            // interval with the same fee.
            let deficit = (available as u128)
                .checked_sub(low_cost)
                .ok_or(ErrorCode::FeeMathOverflow)?;
            let candidate = (low as u128).checked_add(deficit).ok_or(ErrorCode::FeeMathOverflow)?;
            if candidate > low as u128 && candidate < high as u128 {
                let candidate = u64::try_from(candidate).map_err(|_| ErrorCode::FeeMathOverflow)?;
                let (candidate_cost, candidate_cost_saturated) = divergence_potential.total_cost_probe(candidate)?;
                if candidate_cost_saturated || candidate_cost > available as u128 {
                    high = candidate;
                    high_cost = candidate_cost;
                    high_cost_saturated = candidate_cost_saturated;
                } else {
                    require_eq!(candidate_cost, available as u128, ErrorCode::BrokenInvariant);
                    low = candidate;
                    low_cost = candidate_cost;
                }
            }
        } else {
            high = probe;
            high_cost = probe_cost;
            high_cost_saturated = probe_cost_saturated;
            probe_from_high = true;
        }
    }

    // Never silently accept an iteration-limit approximation. Exact total
    // cost proves the root directly; an adjacent infeasible upper endpoint
    // proves that `low` is the maximal feasible raw-token input.
    require!(
        low_cost == available as u128 || high - low <= 1,
        ErrorCode::FeeMathOverflow
    );

    // Charging the residual as divergence surcharge is exact whenever the
    // discrete equation has a root. Across an unavoidable raw-token gap it is
    // pool-favoring by less than the gap and always leaves the selected
    // executable endpoint fully funded.
    let surcharge = available.checked_sub(low).ok_or(ErrorCode::FeeMathOverflow)?;
    require!(surcharge > 0, ErrorCode::BrokenInvariant);
    Ok(surcharge)
}

#[cfg(test)]
fn divergence_total_cost(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    executable_input: u64,
    input_decimals: u8,
    coefficient_nad: u64,
) -> Result<u128> {
    let prepared =
        prepare_outward_divergence_potential(center_input_reserve_nad, start_input_reserve_nad, coefficient_nad)?;
    Ok(divergence_total_cost_probe(&prepared, executable_input, input_decimals)?.0)
}

#[cfg(test)]
fn divergence_total_cost_probe(
    prepared: &PreparedOutwardDivergencePotential,
    executable_input: u64,
    input_decimals: u8,
) -> Result<(u128, bool)> {
    if executable_input == 0 {
        return Ok((executable_input as u128, false));
    }
    let executable_input_nad = normalize_to_nad(executable_input as u128, input_decimals)?;
    let end_input_reserve_nad = prepared
        .start_input_reserve_nad()
        .checked_add(executable_input_nad)
        .ok_or(ErrorCode::ReserveOverflow)?;
    let (fee, fee_saturated) =
        outward_divergence_fee_raw_saturating_prepared(prepared, end_input_reserve_nad, input_decimals)?;
    if fee_saturated {
        return Ok((u128::MAX, true));
    }
    match (executable_input as u128).checked_add(fee) {
        Some(cost) => Ok((cost, false)),
        None => Ok((u128::MAX, true)),
    }
}

#[cfg(test)]
fn divergence_fee_for_executable_input(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    executable_input: u64,
    input_decimals: u8,
    coefficient_nad: u64,
) -> Result<u64> {
    if executable_input == 0 || coefficient_nad == 0 {
        return Ok(0);
    }
    let prepared =
        prepare_outward_divergence_potential(center_input_reserve_nad, start_input_reserve_nad, coefficient_nad)?;
    let (cost, saturated) = divergence_total_cost_probe(&prepared, executable_input, input_decimals)?;
    require!(!saturated, ErrorCode::FeeMathOverflow);
    let fee = cost
        .checked_sub(executable_input as u128)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    u64::try_from(fee).map_err(|_| ErrorCode::FeeMathOverflow.into())
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/swap_engine.rs");
}
