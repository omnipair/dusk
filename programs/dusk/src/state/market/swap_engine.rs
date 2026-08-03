use anchor_lang::prelude::*;
#[cfg(test)]
use std::cell::Cell;

use super::{CurveReservesNad, CurveStateCertificate, Market, MarketAsset};
use crate::{
    constants::{BPS_DENOMINATOR, NAD, NAD_DECIMALS},
    errors::ErrorCode,
    math::{
        normalize_to_nad, outward_divergence_fee_raw_saturating_prepared, outward_divergence_marginal_rate_nad,
        prepare_common_divergence_potential, prepare_outward_divergence_potential, quote_dynamic_fee,
        volatility_after_success_nad, DynamicFeeConfig, DynamicFeePath, DynamicFeePreState,
        PreparedCommonDivergencePotential, PreparedOutwardDivergencePotential,
    },
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
enum PreparedSwapDivergencePotential {
    Raw {
        potential: PreparedOutwardDivergencePotential,
        center_input_reserve_nad: u128,
        start_input_reserve_nad: u128,
        input_decimals: u8,
        coefficient_nad: u64,
    },
    Common(PreparedCommonDivergencePotential),
}

impl PreparedSwapDivergencePotential {
    fn total_cost_probe(self, executable_input: u64) -> Result<(u128, bool)> {
        match self {
            Self::Raw {
                potential,
                input_decimals,
                ..
            } => divergence_total_cost_probe(&potential, executable_input, input_decimals),
            Self::Common(potential) => {
                if executable_input == 0 {
                    return Ok((0, false));
                }
                let (fee, saturated) = potential.fee_raw_saturating(executable_input)?;
                if saturated {
                    return Ok((u128::MAX, true));
                }
                match (executable_input as u128).checked_add(fee) {
                    Some(cost) => Ok((cost, false)),
                    None => Ok((u128::MAX, true)),
                }
            }
        }
    }

    fn marginal_rate_nad(self, executable_input: u64) -> Result<u64> {
        match self {
            Self::Raw {
                center_input_reserve_nad,
                start_input_reserve_nad,
                input_decimals,
                coefficient_nad,
                ..
            } => {
                let origin_nad = normalize_to_nad(executable_input as u128, input_decimals)?;
                let origin_reserve_nad = start_input_reserve_nad
                    .checked_add(origin_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                outward_divergence_marginal_rate_nad(center_input_reserve_nad, origin_reserve_nad, coefficient_nad)
            }
            Self::Common(potential) => potential.marginal_rate_nad(executable_input),
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
    endpoint_certificates: Option<AmmSwapEndpointCertificates>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AmmSwapEndpointCertificates {
    trade: CurveStateCertificate,
    reserve: CurveStateCertificate,
}

impl AmmSwapQuote {
    pub(crate) fn trade_endpoint_certificate(&self) -> Result<CurveStateCertificate> {
        self.endpoint_certificates
            .map(|certificates| certificates.trade)
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }

    pub(crate) fn reserve_endpoint_certificate(&self) -> Result<CurveStateCertificate> {
        self.endpoint_certificates
            .map(|certificates| certificates.reserve)
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }

    /// Leverage receipts intentionally contain only ABI-visible quote fields.
    /// Reconstructed quotes are valid for reserve-overlay simulations, but may
    /// never enter a certificate-reusing execution path.
    #[allow(clippy::too_many_arguments)]
    pub(crate) const fn new_uncertified(
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
            endpoint_certificates: None,
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
}

impl Market {
    /// Net input used by the hLP pre-solver. It includes base and already-known
    /// volatility fees, but intentionally omits divergence. Because the final
    /// divergence fee can only reduce input, the pre-solve endpoint is a
    /// conservative outward path for the second pass.
    pub(crate) fn preliminary_swap_inputs(
        &self,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<PreliminarySwapInputs> {
        require!(reserve_credit > 0, ErrorCode::AmountZero);
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        self.preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state.center_price_nad, pre_state)
    }

    fn preliminary_swap_inputs_for_state(
        &self,
        reserve_credit: u64,
        current_slot: u64,
        start_price_nad: u64,
        pre_state: DynamicFeePreState,
    ) -> Result<PreliminarySwapInputs> {
        require!(reserve_credit > 0, ErrorCode::AmountZero);
        let mut config = self.dynamic_fee_config()?;
        config.divergence_coefficient_nad = 0;
        let preliminary = quote_dynamic_fee(
            config,
            pre_state,
            DynamicFeePath {
                amount_in: reserve_credit,
                start_price_nad,
                end_price_nad: start_price_nad,
                current_slot,
                divergence_surcharge_amount: 0,
            },
        )?;
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
        })
    }

    /// Deterministic two-pass quote. The caller may run the hLP pre-solver with
    /// `preliminary_swap_input` first; this method then freezes the resulting
    /// curve state, obtains a conservative no-divergence endpoint, charges the
    /// path fee, and quotes once more with the final net input.
    pub(crate) fn quote_amm_swap(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<AmmSwapQuote> {
        self.quote_amm_swap_for_reserves_nad(
            asset_in,
            reserve_credit,
            current_slot,
            self.curve_reserves_nad()?,
            self.dynamic_fee_pre_state(current_slot)?,
        )
    }

    /// Quotes a second trade against the executable reserves left by `first`
    /// without mutating EMA, protected-liquidity, or ramp state. This is used
    /// by leverage health checks to price the exact unwind that would follow a
    /// successful opening/increase/decrease trade.
    pub(crate) fn quote_amm_swap_after(
        &self,
        first: &AmmSwapQuote,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<AmmSwapQuote> {
        let reserves = self.curve_reserves_after_amm_swap_nad(first)?;
        let pre_state = DynamicFeePreState {
            center_price_nad: self.current_curve_center_price_nad()?,
            volatility_accumulator_nad: first.post_success_volatility_nad,
            volatility_last_update_slot: current_slot,
        };
        self.quote_amm_swap_for_reserves_nad(asset_in, reserve_credit, current_slot, reserves, pre_state)
    }

    fn quote_amm_swap_for_reserves_nad(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
        reserves: CurveReservesNad,
        pre_state: DynamicFeePreState,
    ) -> Result<AmmSwapQuote> {
        // Preliminary fee input depends only on the frozen accumulator. The
        // invariant-coordinate divergence potential needs starting D and the
        // input-reserve displacement, not a provisional output quote or its
        // marginal prices. Avoiding that redundant CONCENTRATED quote removes an entire
        // reserve solve plus two marginal-price proofs from every swap.
        let preliminary_input = self
            .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state.center_price_nad, pre_state)?
            .amount_in_for_quote;
        let config = self.dynamic_fee_config()?;
        let prepared = self.prepare_curve_for_reserves_nad(reserves, pre_state.center_price_nad, current_slot)?;
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
        let divergence_surcharge_amount = if prepared.peak_depth_nad() == 0 {
            let center_input_reserve_nad =
                center_input_reserve_nad(asset_in, invariant_d_nad, pre_state.center_price_nad)?;
            implicit_divergence_surcharge_amount(
                center_input_reserve_nad,
                start_input_reserve_nad,
                preliminary_input,
                input_decimals,
                config.divergence_coefficient_nad,
            )?
        } else {
            let balanced_common_nad = invariant_d_nad / 2;
            require!(balanced_common_nad > 0, ErrorCode::BrokenInvariant);
            let (start_common_nad, common_rate_nad) = match asset_in {
                MarketAsset::Base => (prepared.base_common_nad(), pre_state.center_price_nad),
                MarketAsset::Quote => (prepared.quote_common_nad(), NAD),
            };
            let common = prepare_common_divergence_potential(
                balanced_common_nad,
                start_input_reserve_nad,
                start_common_nad,
                common_rate_nad,
                input_decimals,
                config.divergence_coefficient_nad,
            )?;
            implicit_common_divergence_surcharge_amount(common, preliminary_input)?
        };
        let dynamic = quote_dynamic_fee(
            config,
            pre_state,
            DynamicFeePath {
                amount_in: reserve_credit,
                // The final executable path is committed below. Fee amount
                // calculation consumes only the already-decayed accumulator.
                start_price_nad: pre_state.center_price_nad,
                end_price_nad: pre_state.center_price_nad,
                current_slot,
                divergence_surcharge_amount,
            },
        )?;
        let amount_in_for_quote = reserve_credit
            .checked_sub(dynamic.total_fee_amount)
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
        let trade_endpoint = final_curve.endpoint_certificate();
        let reserve_endpoint = if retained_surcharge == 0 {
            trade_endpoint
        } else {
            let mut endpoint_reserves = trade_endpoint.reserves();
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
            self.certify_curve_successor_nad(trade_endpoint, endpoint_reserves, current_slot)?
        };
        let post_success_volatility_nad = volatility_after_success_nad(
            dynamic.decayed_volatility_nad,
            final_curve.start_price_nad,
            final_curve.end_price_nad,
            self.config.amm.volatility_shock_cap_nad,
            self.config.amm.volatility_cap_nad,
        )?;
        let reserve_end_price_nad = u64::try_from(reserve_endpoint.certified_evaluation().marginal_price_nad)
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
            endpoint_certificates: Some(AmmSwapEndpointCertificates {
                trade: trade_endpoint,
                reserve: reserve_endpoint,
            }),
        })
    }

    fn curve_reserves_after_amm_swap_nad(&self, quote: &AmmSwapQuote) -> Result<CurveReservesNad> {
        require_eq!(
            quote.fee.reserve_input_credit,
            quote
                .fee
                .amount_in_for_quote
                .checked_add(quote.fee.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        let mut reserves = self.curve_reserves_nad()?;
        let input_nad = normalize_to_nad(
            quote.fee.reserve_input_credit as u128,
            self.side(quote.asset_in).asset_decimals,
        )?;
        let output_nad = normalize_to_nad(
            quote.amount_out as u128,
            self.side(quote.asset_in.opposite()).asset_decimals,
        )?;
        match quote.asset_in {
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
        Ok(reserves)
    }

    fn dynamic_fee_config(&self) -> Result<DynamicFeeConfig> {
        let base_fee_rate_nad = bps_to_nad(self.config.swap_fee_bps)?;
        Ok(DynamicFeeConfig {
            base_fee_rate_nad,
            divergence_coefficient_nad: self.config.amm.divergence_fee_coefficient_nad,
            volatility_coefficient_nad: self.config.amm.volatility_fee_coefficient_nad,
            volatility_half_life_ms: self.config.amm.volatility_half_life_ms,
            volatility_shock_cap_nad: self.config.amm.volatility_shock_cap_nad,
            volatility_accumulator_cap_nad: self.config.amm.volatility_cap_nad,
        })
    }

    fn dynamic_fee_pre_state(&self, current_slot: u64) -> Result<DynamicFeePreState> {
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

fn center_input_reserve_nad(asset_in: MarketAsset, invariant_d_nad: u128, center_price_nad: u64) -> Result<u128> {
    require!(invariant_d_nad > 0 && center_price_nad > 0, ErrorCode::InvalidArgument);
    match asset_in {
        MarketAsset::Base => invariant_d_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(2_u128.checked_mul(center_price_nad as u128)?))
            .filter(|value| *value > 0)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into()),
        MarketAsset::Quote => invariant_d_nad
            .checked_div(2)
            .filter(|value| *value > 0)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into()),
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
fn implicit_divergence_surcharge_amount(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    available: u64,
    input_decimals: u8,
    coefficient_nad: u64,
) -> Result<u64> {
    require!(available > 0, ErrorCode::AmountZero);
    require!(input_decimals <= NAD_DECIMALS, ErrorCode::UnsupportedAssetDecimals);
    if coefficient_nad == 0 {
        return Ok(0);
    }

    let potential = PreparedSwapDivergencePotential::Raw {
        potential: prepare_outward_divergence_potential(
            center_input_reserve_nad,
            start_input_reserve_nad,
            coefficient_nad,
        )?,
        center_input_reserve_nad,
        start_input_reserve_nad,
        input_decimals,
        coefficient_nad,
    };
    implicit_divergence_surcharge_amount_core(potential, available)
}

fn implicit_common_divergence_surcharge_amount(
    potential: PreparedCommonDivergencePotential,
    available: u64,
) -> Result<u64> {
    require!(available > 0, ErrorCode::AmountZero);
    implicit_divergence_surcharge_amount_core(PreparedSwapDivergencePotential::Common(potential), available)
}

fn implicit_divergence_surcharge_amount_core(
    divergence_potential: PreparedSwapDivergencePotential,
    available: u64,
) -> Result<u64> {
    debug_assert!(available > 0);

    // Zero executable input is always feasible. Gross input itself is either
    // exactly fee-free (and therefore the answer) or an infeasible endpoint.
    // Unlike the former bounded-rate potential, the new toll may exceed gross,
    // so no subtraction from `available` is used to manufacture a lower bound.
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
                let marginal_rate_nad = divergence_potential.marginal_rate_nad(origin)?;
                let derivative_nad = (NAD as u128)
                    .checked_add(marginal_rate_nad as u128)
                    .ok_or(ErrorCode::FeeMathOverflow)?;
                let step = ceil_scaled_residual_step(residual, derivative_nad)?.max(1);
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

/// Computes `ceil(residual * NAD / derivative_nad)` without multiplying two
/// arbitrary u128 values. `derivative_nad >= NAD`, so the whole-number part
/// cannot exceed `residual`; the remainder product is bounded by a saturated
/// u64 marginal times NAD.
fn ceil_scaled_residual_step(residual: u128, derivative_nad: u128) -> Result<u64> {
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
    let step = whole.checked_add(fractional).ok_or(ErrorCode::FeeMathOverflow)?;
    Ok(u64::try_from(step).unwrap_or(u64::MAX))
}

fn bps_to_nad(bps: u16) -> Result<u64> {
    let value = (bps as u128)
        .checked_mul(NAD as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::FeeMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::FeeMathOverflow.into())
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/swap_engine.rs");
}
