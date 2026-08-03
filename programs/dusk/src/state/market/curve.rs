use anchor_lang::prelude::*;

use super::{AmmCurveParameters, Market, MarketAsset};
#[cfg(test)]
use crate::math::concentrated_evaluate;
use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::{
        concentrated_marginal_price_nad, concentrated_prepare_continuous_successor_from_bracket,
        concentrated_prepare_curve, concentrated_prepare_curve_with_hint, concentrated_quote_exact_out,
        concentrated_restore_prepared_curve_from_bracket, denormalize_from_nad_ceil, denormalize_from_nad_floor,
        normalize_to_nad, ConcentratedEvaluation, ConcentratedPreparedCurve, ConcentratedSwapDirection,
    },
};

/// Executable AMM inventory. Unlike `live_reserve`, these coordinates exclude
/// accrued-but-unpaid lending interest because interest is claimable yield,
/// not compounding swap principal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CurveReservesNad {
    pub base: u128,
    pub quote: u128,
}

/// Opaque proof that a full Dusk Concentrated AMM evaluation belongs to one exact executable
/// curve state.
///
/// Reserve coordinates are normalized from the raw amounts that will actually
/// be credited/debited on-chain. Private identity fields prevent callers from
/// pairing the cached evaluation with different reserves, center, or curve
/// parameters merely to avoid a solve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CurveStateCertificate {
    reserves: CurveReservesNad,
    center_price_nad: u64,
    parameters: AmmCurveParameters,
    evaluation: ConcentratedEvaluation,
}

impl CurveStateCertificate {
    pub(crate) fn matches_market(self, market: &Market, current_slot: u64) -> Result<bool> {
        Ok(self.reserves == market.curve_reserves_nad()?
            && self.center_price_nad == market.current_curve_center_price_nad()?
            && self.parameters == market.current_curve_parameters(current_slot))
    }

    pub(crate) fn evaluation_if_matches(
        self,
        market: &Market,
        current_slot: u64,
    ) -> Result<Option<ConcentratedEvaluation>> {
        Ok(self.matches_market(market, current_slot)?.then_some(self.evaluation))
    }

    pub(crate) fn validated_evaluation(self, market: &Market, current_slot: u64) -> Result<ConcentratedEvaluation> {
        self.evaluation_if_matches(market, current_slot)?
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }

    pub(crate) const fn reserves(self) -> CurveReservesNad {
        self.reserves
    }

    /// Returns the evaluation carried by this opaque, identity-bound proof.
    /// Callers that need to apply it to mutable market state must still use
    /// `validated_evaluation`; read-only projections may consume it directly.
    pub(crate) const fn certified_evaluation(self) -> ConcentratedEvaluation {
        self.evaluation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CurveQuote {
    pub amount_in: u64,
    pub amount_out: u64,
    pub start_price_nad: u64,
    pub end_price_nad: u64,
    endpoint: CurveStateCertificate,
}

impl CurveQuote {
    pub(crate) const fn endpoint_certificate(self) -> CurveStateCertificate {
        self.endpoint
    }
}

impl Market {
    /// Aggregate accrued interest which has not yet been paid into the
    /// non-compounding interest vault. Debt-share rounding can momentarily put
    /// tracked principal above computed debt, so principal is clamped first.
    pub(crate) fn unrealized_interest(&self, asset: MarketAsset) -> Result<u128> {
        let (fixed_debt, fixed_principal, isolated_debt, isolated_principal) = match asset {
            MarketAsset::Base => (
                self.debt.fixed_base_debt()?,
                self.debt.fixed_base_principal,
                self.debt.isolated_debt(MarketAsset::Base)?,
                self.debt.isolated_base_principal,
            ),
            MarketAsset::Quote => (
                self.debt.fixed_quote_debt()?,
                self.debt.fixed_quote_principal,
                self.debt.isolated_debt(MarketAsset::Quote)?,
                self.debt.isolated_quote_principal,
            ),
        };
        fixed_debt
            .checked_sub(fixed_principal.min(fixed_debt))
            .and_then(|fixed_interest| {
                isolated_debt
                    .checked_sub(isolated_principal.min(isolated_debt))
                    .and_then(|isolated_interest| fixed_interest.checked_add(isolated_interest))
            })
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn curve_reserve(&self, asset: MarketAsset) -> Result<u64> {
        let live_reserve = self.side(asset).reserves.live_reserve as u128;
        let curve_reserve = live_reserve
            .checked_sub(self.unrealized_interest(asset)?)
            .ok_or(ErrorCode::BrokenInvariant)?;
        u64::try_from(curve_reserve).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn curve_reserves_nad(&self) -> Result<CurveReservesNad> {
        Ok(CurveReservesNad {
            base: normalize_to_nad(
                self.curve_reserve(MarketAsset::Base)? as u128,
                self.base_side.asset_decimals,
            )?,
            quote: normalize_to_nad(
                self.curve_reserve(MarketAsset::Quote)? as u128,
                self.quote_side.asset_decimals,
            )?,
        })
    }

    pub(crate) fn current_curve_parameters(&self, current_slot: u64) -> AmmCurveParameters {
        self.amm.effective_curve_parameters(&self.config.amm, current_slot)
    }

    /// Until first liquidity initializes AMM state, the reserve ratio is the
    /// only meaningful center. This makes a configured concentrated pool begin
    /// balanced without any external price input.
    pub(crate) fn current_curve_center_price_nad(&self) -> Result<u64> {
        if self.amm.initialized {
            require!(self.amm.center_price_nad > 0, ErrorCode::BrokenInvariant);
            return Ok(self.amm.center_price_nad);
        }
        let reserves = self.curve_reserves_nad()?;
        require!(
            reserves.base > 0 && reserves.quote > 0,
            ErrorCode::InsufficientLiquidity
        );
        let center = reserves
            .quote
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(reserves.base))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(center).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn evaluate_current_curve(&self, current_slot: u64) -> Result<ConcentratedEvaluation> {
        let reserves = self.curve_reserves_nad()?;
        self.prepare_curve_for_reserves_nad(reserves, self.current_curve_center_price_nad()?, current_slot)?
            .evaluation()
    }

    /// Reconstructs a full identity-bound certificate from the exact bracket
    /// already committed for the current reserves/center/parameters. This is
    /// used immediately after a funded ramp or recenter, whose candidate solve
    /// produced and committed both endpoints atomically.
    pub(crate) fn certify_current_curve_from_persisted_bracket(
        &self,
        current_slot: u64,
    ) -> Result<CurveStateCertificate> {
        let reserves = self.curve_reserves_nad()?;
        let center_price_nad = self.current_curve_center_price_nad()?;
        let parameters = self.current_curve_parameters(current_slot);
        let prepared = concentrated_restore_prepared_curve_from_bracket(
            reserves.base,
            reserves.quote,
            center_price_nad as u128,
            parameters.peak_depth_nad as u128,
            parameters.imbalance_scale_nad as u128,
            self.amm.invariant_d_nad,
            self.amm.invariant_d_high_nad,
        )?;
        self.certify_prepared_curve(prepared, current_slot)
    }

    /// Certifies another point on the same applied curve using an opaque
    /// predecessor's D only as a performance hint. This is used for the
    /// post-retention reserve endpoint; the concentrated solver still rebuilds and
    /// proves its complete global bracket.
    pub(crate) fn certify_curve_successor_nad(
        &self,
        predecessor: CurveStateCertificate,
        reserves: CurveReservesNad,
        current_slot: u64,
    ) -> Result<CurveStateCertificate> {
        let center_price_nad = self.current_curve_center_price_nad()?;
        let parameters = self.current_curve_parameters(current_slot);
        require!(
            predecessor.center_price_nad == center_price_nad && predecessor.parameters == parameters,
            ErrorCode::BrokenInvariant
        );
        let prior = predecessor.reserves;
        let base_only = reserves.base >= prior.base && reserves.quote == prior.quote;
        let quote_only = reserves.quote >= prior.quote && reserves.base == prior.base;
        require!(base_only ^ quote_only, ErrorCode::BrokenInvariant);
        let prepared = concentrated_prepare_continuous_successor_from_bracket(
            prior.base,
            prior.quote,
            reserves.base,
            reserves.quote,
            center_price_nad as u128,
            parameters.peak_depth_nad as u128,
            parameters.imbalance_scale_nad as u128,
            predecessor.evaluation.invariant_d,
            predecessor.evaluation.invariant_d_high,
        )?;
        let evaluation = prepared.continuous_successor_evaluation()?;
        Ok(CurveStateCertificate {
            reserves: CurveReservesNad {
                base: prepared.base_reserve_nad(),
                quote: prepared.quote_reserve_nad(),
            },
            center_price_nad,
            parameters,
            evaluation,
        })
    }

    /// Converts a prepared concentrated point into an opaque market-bound certificate.
    ///
    /// `prepare_successor` rebuilds the full global invariant sign bracket;
    /// its prior D is only a Newton hint. These identity checks ensure the
    /// resulting proof cannot be paired with a different live center or
    /// applied parameter point.
    fn certify_prepared_curve(
        &self,
        prepared: ConcentratedPreparedCurve,
        current_slot: u64,
    ) -> Result<CurveStateCertificate> {
        let mut certificate = self.certify_prepared_curve_identity(prepared, current_slot)?;
        certificate.evaluation = prepared.evaluation()?;
        Ok(certificate)
    }

    fn certify_prepared_curve_identity(
        &self,
        prepared: ConcentratedPreparedCurve,
        current_slot: u64,
    ) -> Result<CurveStateCertificate> {
        let center_price_nad = u64::try_from(prepared.center_price_nad()).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let parameters = AmmCurveParameters {
            peak_depth_nad: u64::try_from(prepared.peak_depth_nad()).map_err(|_| ErrorCode::MarketMathOverflow)?,
            imbalance_scale_nad: u64::try_from(prepared.imbalance_scale_nad())
                .map_err(|_| ErrorCode::MarketMathOverflow)?,
        };
        require_eq!(
            center_price_nad,
            self.current_curve_center_price_nad()?,
            ErrorCode::BrokenInvariant
        );
        require!(
            parameters == self.current_curve_parameters(current_slot),
            ErrorCode::BrokenInvariant
        );
        Ok(CurveStateCertificate {
            reserves: CurveReservesNad {
                base: prepared.base_reserve_nad(),
                quote: prepared.quote_reserve_nad(),
            },
            center_price_nad,
            parameters,
            evaluation: ConcentratedEvaluation {
                invariant_d: prepared.invariant_d(),
                invariant_d_high: prepared.invariant_bracket().1,
                balanced_equivalent_q: 0,
                marginal_price_nad: 0,
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn evaluate_curve_candidate(
        &self,
        center_price_nad: u64,
        parameters: AmmCurveParameters,
    ) -> Result<ConcentratedEvaluation> {
        let reserves = self.curve_reserves_nad()?;
        concentrated_evaluate(
            reserves.base,
            reserves.quote,
            center_price_nad as u128,
            parameters.peak_depth_nad as u128,
            parameters.imbalance_scale_nad as u128,
        )
    }

    pub(crate) fn curve_q_per_share_nad(&self, balanced_equivalent_q_nad: u128) -> Result<u128> {
        let supply = self.base_side.shares.ylp_supply;
        require_eq!(supply, self.quote_side.shares.ylp_supply, ErrorCode::BrokenInvariant);
        require!(supply > 0, ErrorCode::SupplyUnderflow);
        let supply_nad = normalize_to_nad(supply as u128, self.base_side.asset_decimals)?;
        balanced_equivalent_q_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(supply_nad))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn curve_marginal_price_nad(&self, current_slot: u64) -> Result<u64> {
        let reserves = self.curve_reserves_nad()?;
        if let Some(price_nad) = self.cached_exact_concentrated_start_price_nad(reserves, current_slot)? {
            return Ok(price_nad);
        }
        self.curve_marginal_price_for_reserves_nad(reserves, current_slot)
    }

    pub(crate) fn curve_marginal_price_for_reserves_nad(
        &self,
        reserves: CurveReservesNad,
        current_slot: u64,
    ) -> Result<u64> {
        let parameters = self.current_curve_parameters(current_slot);
        let price = concentrated_marginal_price_nad(
            reserves.base,
            reserves.quote,
            self.current_curve_center_price_nad()? as u128,
            parameters.peak_depth_nad as u128,
            parameters.imbalance_scale_nad as u128,
        )?;
        u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    /// Returns a persisted exact marginal only for the identical concentrated
    /// start state. CPMM deliberately keeps its direct reserve-ratio path.
    pub(crate) fn cached_exact_concentrated_start_price_nad(
        &self,
        reserves: CurveReservesNad,
        current_slot: u64,
    ) -> Result<Option<u64>> {
        let parameters = self.current_curve_parameters(current_slot);
        if self.risk.cached_spot_base_price_nad == 0 {
            return Ok(None);
        }
        let center_price_nad = self.current_curve_center_price_nad()?;
        Ok(self
            .exact_concentrated_observation_matches(reserves, center_price_nad, parameters)
            .then_some(self.risk.cached_spot_base_price_nad))
    }

    pub(crate) fn curve_price_for_asset_nad(&self, asset: MarketAsset, current_slot: u64) -> Result<u64> {
        let base_price = self.curve_marginal_price_nad(current_slot)?;
        match asset {
            MarketAsset::Base => Ok(base_price),
            MarketAsset::Quote => {
                require!(base_price > 0, ErrorCode::InvalidSettlementPrice);
                let inverse = (NAD as u128)
                    .checked_mul(NAD as u128)
                    .and_then(|value| value.checked_div(base_price as u128))
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(inverse).map_err(|_| ErrorCode::MarketMathOverflow.into())
            }
        }
    }

    pub(crate) fn quote_curve_exact_in(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
    ) -> Result<CurveQuote> {
        let reserves = self.curve_reserves_nad()?;
        self.quote_curve_exact_in_for_reserves_nad(asset_in, amount_in, current_slot, reserves)
    }

    pub(crate) fn quote_curve_exact_in_for_reserves_nad(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
        reserves: CurveReservesNad,
    ) -> Result<CurveQuote> {
        let prepared =
            self.prepare_curve_for_reserves_nad(reserves, self.current_curve_center_price_nad()?, current_slot)?;
        self.quote_curve_exact_in_for_prepared_nad(asset_in, amount_in, prepared, current_slot)
    }

    /// Builds the one start-state certificate shared by divergence, output,
    /// and starting-price calculations.
    pub(crate) fn prepare_curve_for_reserves_nad(
        &self,
        reserves: CurveReservesNad,
        center_price_nad: u64,
        current_slot: u64,
    ) -> Result<ConcentratedPreparedCurve> {
        let parameters = self.current_curve_parameters(current_slot);
        if self.exact_concentrated_observation_matches(reserves, center_price_nad, parameters) {
            return concentrated_restore_prepared_curve_from_bracket(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.imbalance_scale_nad as u128,
                self.amm.invariant_d_nad,
                self.amm.invariant_d_high_nad,
            );
        }
        if !parameters.is_cpmm() && self.amm.initialized && self.amm.invariant_d_nad > 0 {
            // The hint is useful both for the exact live state and for
            // sequential overlay quotes. It never narrows the authoritative
            // global bracket, so stale/different overlay inventory can only
            // reduce the optimization, not change the certified result.
            concentrated_prepare_curve_with_hint(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.imbalance_scale_nad as u128,
                self.amm.invariant_d_nad,
            )
        } else {
            concentrated_prepare_curve(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.imbalance_scale_nad as u128,
            )
        }
    }

    fn exact_concentrated_observation_matches(
        &self,
        reserves: CurveReservesNad,
        center_price_nad: u64,
        parameters: AmmCurveParameters,
    ) -> bool {
        self.amm.initialized
            && !parameters.is_cpmm()
            && self.amm.invariant_d_nad > 0
            && self.amm.invariant_d_high_nad >= self.amm.invariant_d_nad
            && self
                .amm
                .exact_curve_observation
                .matches(reserves.base, reserves.quote, center_price_nad, parameters)
    }

    pub(crate) fn quote_curve_exact_in_for_prepared_nad(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        prepared: ConcentratedPreparedCurve,
        current_slot: u64,
    ) -> Result<CurveQuote> {
        require!(amount_in > 0, ErrorCode::AmountZero);
        let amount_in_nad = normalize_to_nad(amount_in as u128, self.side(asset_in).asset_decimals)?;
        require!(amount_in_nad > 0, ErrorCode::AmountZero);
        let solved_amount_out_nad = prepared.quote_exact_in(amount_in_nad, direction(asset_in))?;
        let amount_out =
            denormalize_from_nad_floor(solved_amount_out_nad, self.side(asset_in.opposite()).asset_decimals)?;
        require!(amount_out > 0, ErrorCode::InsufficientOutputAmount);
        // The solver coordinate may contain sub-raw-token dust. Execution
        // floors to `amount_out`, so endpoint price, D, and Q must all use the
        // normalized raw debit rather than the larger unexecutable solve.
        let executable_amount_out_nad =
            normalize_to_nad(amount_out as u128, self.side(asset_in.opposite()).asset_decimals)?;
        require!(executable_amount_out_nad > 0, ErrorCode::InsufficientOutputAmount);

        let prepared_reserves = CurveReservesNad {
            base: prepared.base_reserve_nad(),
            quote: prepared.quote_reserve_nad(),
        };
        let start_price_nad = if let Some(cached_price_nad) =
            self.cached_exact_concentrated_start_price_nad(prepared_reserves, current_slot)?
        {
            cached_price_nad
        } else {
            u64::try_from(prepared.marginal_price_nad()?).map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        let (base_after, quote_after) = match asset_in {
            MarketAsset::Base => (
                prepared
                    .base_reserve_nad()
                    .checked_add(amount_in_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?,
                prepared
                    .quote_reserve_nad()
                    .checked_sub(executable_amount_out_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
            ),
            MarketAsset::Quote => (
                prepared
                    .base_reserve_nad()
                    .checked_sub(executable_amount_out_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
                prepared
                    .quote_reserve_nad()
                    .checked_add(amount_in_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?,
            ),
        };
        let successor = prepared.prepare_successor(base_after, quote_after)?;
        let mut endpoint = self.certify_prepared_curve_identity(successor, current_slot)?;
        endpoint.evaluation = successor.continuous_successor_evaluation()?;

        Ok(CurveQuote {
            amount_in,
            amount_out,
            start_price_nad,
            end_price_nad: u64::try_from(endpoint.evaluation.marginal_price_nad)
                .map_err(|_| ErrorCode::MarketMathOverflow)?,
            endpoint,
        })
    }

    pub(crate) fn quote_curve_exact_out(
        &self,
        asset_out: MarketAsset,
        amount_out: u64,
        current_slot: u64,
    ) -> Result<u64> {
        require!(amount_out > 0, ErrorCode::AmountZero);
        let asset_in = asset_out.opposite();
        let reserves = self.curve_reserves_nad()?;
        let parameters = self.current_curve_parameters(current_slot);
        let amount_out_nad = normalize_to_nad(amount_out as u128, self.side(asset_out).asset_decimals)?;
        let amount_in_nad = concentrated_quote_exact_out(
            reserves.base,
            reserves.quote,
            amount_out_nad,
            direction(asset_in),
            self.current_curve_center_price_nad()? as u128,
            parameters.peak_depth_nad as u128,
            parameters.imbalance_scale_nad as u128,
        )?;
        denormalize_from_nad_ceil(amount_in_nad, self.side(asset_in).asset_decimals)
    }
}

const fn direction(asset_in: MarketAsset) -> ConcentratedSwapDirection {
    match asset_in {
        MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
        MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/curve.rs");
}
