use anchor_lang::prelude::*;

use super::{AmmCurveParameters, Market, MarketAsset};
#[cfg(test)]
use crate::math::concentrated_evaluate;
use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::{
        concentrated_prepare_curve, concentrated_prepare_curve_cached, concentrated_prepare_curve_seeded_cached,
        denormalize_from_nad_floor, normalize_to_nad, ConcentratedEvaluation, ConcentratedInvariantSeed,
        ConcentratedPreparedCurve, ConcentratedSwapDirection, CONCENTRATED_MATH_REVISION,
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

/// One fully evaluated executable curve state produced by the quote pipeline.
///
/// Reserve coordinates are normalized from the raw amounts that will actually
/// be credited/debited on-chain. Private identity fields prevent callers from
/// pairing the cached evaluation with different reserves, center, or curve
/// parameters merely to avoid a solve. This is an ephemeral plan value, not a
/// second persisted invariant proof hierarchy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CurveCheckpoint {
    pub(crate) reserves: CurveReservesNad,
    center_price_nad: u64,
    parameters: AmmCurveParameters,
    evaluation: ConcentratedEvaluation,
}

impl CurveCheckpoint {
    #[cfg(test)]
    pub(crate) fn evaluation_if_matches(
        self,
        market: &Market,
        current_slot: u64,
    ) -> Result<Option<ConcentratedEvaluation>> {
        Ok((self.reserves == market.curve_reserves_nad()?
            && self.center_price_nad == market.current_curve_center_price_nad()?
            && self.parameters == market.current_curve_parameters(current_slot))
        .then_some(self.evaluation))
    }

    pub(crate) fn validated_evaluation(self, market: &Market, current_slot: u64) -> Result<ConcentratedEvaluation> {
        require!(
            self.reserves == market.curve_reserves_nad()?
                && self.center_price_nad == market.current_curve_center_price_nad()?
                && self.parameters == market.current_curve_parameters(current_slot),
            ErrorCode::BrokenInvariant
        );
        Ok(self.evaluation)
    }

    /// Returns the evaluation carried by this identity-bound checkpoint.
    /// Callers that need to apply it to mutable market state must still use
    /// `validated_evaluation`; read-only projections may consume it directly.
    pub(crate) const fn evaluation(self) -> ConcentratedEvaluation {
        self.evaluation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CurveQuote {
    pub amount_in: u64,
    pub amount_out: u64,
    pub start_price_nad: u64,
    pub end_price_nad: u64,
    pub(crate) endpoint: CurveCheckpoint,
}

impl Market {
    /// Aggregate accrued interest which has not yet been paid into the
    /// non-compounding interest vault. Debt-share rounding can momentarily put
    /// tracked principal above computed debt, so principal is clamped first.
    pub(crate) fn unrealized_interest(&self, asset: MarketAsset) -> Result<u128> {
        let (fixed_debt, fixed_principal, isolated_debt, isolated_principal) = match asset {
            MarketAsset::Base => (
                self.debt.fixed_base_debt()?,
                u128::from(self.debt.fixed_base_principal),
                self.debt.isolated_debt(MarketAsset::Base)?,
                u128::from(self.debt.isolated_base_principal),
            ),
            MarketAsset::Quote => (
                self.debt.fixed_quote_debt()?,
                u128::from(self.debt.fixed_quote_principal),
                self.debt.isolated_debt(MarketAsset::Quote)?,
                u128::from(self.debt.isolated_quote_principal),
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

    /// Binds a prepared point to the currently applied center and parameters.
    pub(crate) fn checkpoint_for_prepared_curve(
        &self,
        prepared: ConcentratedPreparedCurve,
        current_slot: u64,
    ) -> Result<CurveCheckpoint> {
        let center_price_nad = u64::try_from(prepared.center_price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let parameters = AmmCurveParameters {
            peak_depth_nad: u64::try_from(prepared.peak_depth_nad).map_err(|_| ErrorCode::MarketMathOverflow)?,
            fade_scale_nad: u64::try_from(prepared.fade_scale_nad).map_err(|_| ErrorCode::MarketMathOverflow)?,
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
        Ok(CurveCheckpoint {
            reserves: CurveReservesNad {
                base: prepared.base_reserve_nad(),
                quote: prepared.quote_reserve_nad(),
            },
            center_price_nad,
            parameters,
            evaluation: prepared.evaluation()?,
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
            parameters.fade_scale_nad as u128,
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
        let price = self
            .prepare_curve_for_reserves_nad(reserves, self.current_curve_center_price_nad()?, current_slot)?
            .marginal_price_nad()?;
        u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    #[cfg(test)]
    pub(crate) fn quote_curve_exact_in(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
    ) -> Result<CurveQuote> {
        let reserves = self.curve_reserves_nad()?;
        let prepared =
            self.prepare_curve_for_reserves_nad(reserves, self.current_curve_center_price_nad()?, current_slot)?;
        self.quote_curve_exact_in_for_prepared_nad(asset_in, amount_in, prepared, current_slot)
    }

    /// Builds the one start-state preparation shared by divergence, output,
    /// and starting-price calculations.
    pub(crate) fn prepare_curve_for_reserves_nad(
        &self,
        reserves: CurveReservesNad,
        center_price_nad: u64,
        current_slot: u64,
    ) -> Result<ConcentratedPreparedCurve> {
        let parameters = self.current_curve_parameters(current_slot);
        if !parameters.is_cpmm()
            && self.amm.initialized
            && self.amm.invariant_d_nad > 0
            && self.amm.curve_math_revision == CONCENTRATED_MATH_REVISION
        {
            // The hint is useful both for the exact live state and for
            // sequential overlay quotes. It never narrows the authoritative
            // global bracket, so stale/different overlay inventory can only
            // reduce the optimization, not change the canonical result.
            concentrated_prepare_curve_seeded_cached(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
                self.amm.concentrated_geometry_cache,
                ConcentratedInvariantSeed::Hint(self.amm.invariant_d_nad),
            )
        } else if !parameters.is_cpmm()
            && self.amm.initialized
            && self.amm.curve_math_revision == CONCENTRATED_MATH_REVISION
            && self
                .amm
                .concentrated_geometry_cache
                .matches(parameters.peak_depth_nad as u128, parameters.fade_scale_nad as u128)
        {
            concentrated_prepare_curve_cached(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
                self.amm.concentrated_geometry_cache,
            )
        } else {
            concentrated_prepare_curve(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
            )
        }
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
        let solved_amount_out_nad = prepared.quote_exact_in(
            amount_in_nad,
            match asset_in {
                MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
            },
        )?;
        let amount_out =
            denormalize_from_nad_floor(solved_amount_out_nad, self.side(asset_in.opposite()).asset_decimals)?;
        require!(amount_out > 0, ErrorCode::InsufficientOutputAmount);
        // The solver coordinate may contain sub-raw-token dust. Execution
        // floors to `amount_out`, so endpoint price, D, and Q must all use the
        // normalized raw debit rather than the larger unexecutable solve.
        let executable_amount_out_nad =
            normalize_to_nad(amount_out as u128, self.side(asset_in.opposite()).asset_decimals)?;
        require!(executable_amount_out_nad > 0, ErrorCode::InsufficientOutputAmount);

        let start_price_nad =
            u64::try_from(prepared.marginal_price_nad()?).map_err(|_| ErrorCode::MarketMathOverflow)?;
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
        let successor = prepared.prepare_successor(
            base_after,
            quote_after,
            ConcentratedInvariantSeed::Hint(prepared.invariant_d()),
        )?;
        let endpoint = self.checkpoint_for_prepared_curve(successor, current_slot)?;

        Ok(CurveQuote {
            amount_in,
            amount_out,
            start_price_nad,
            end_price_nad: u64::try_from(endpoint.evaluation.marginal_price_nad)
                .map_err(|_| ErrorCode::MarketMathOverflow)?,
            endpoint,
        })
    }
}

#[cfg(test)]
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
