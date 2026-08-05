use anchor_lang::prelude::*;

#[cfg(test)]
use super::CurveCheckpoint;
use super::{Market, MarketAsset, MarketHealth, Risk};
use crate::{
    constants::{BPS_DENOMINATOR, LTV_BUFFER_BPS, NAD},
    errors::ErrorCode,
    math::*,
    shared::math::ceil_div,
    state::{BorrowPosition, Debt},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicBorrowTerms {
    pub max_debt: u64,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
    pub effective_existing_debt_nad: u128,
    pub projected_market_health_bps: u64,
}

#[derive(Clone, Copy)]
struct RiskCurveRequest {
    target_base_price_nad: u128,
    balanced_equivalent_q_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
}

impl Market {
    pub fn market_health(&self) -> Result<MarketHealth> {
        self.market_health_from_risk(&self.risk)
    }

    pub fn market_health_from_risk(&self, risk: &Risk) -> Result<MarketHealth> {
        let total_base_debt_nad = self.total_fixed_debt_nad(MarketAsset::Base)?;
        let total_quote_debt_nad = self.total_fixed_debt_nad(MarketAsset::Quote)?;
        let (effective_base_debt_nad, base_debt_health_bps) = self.global_side_health(
            MarketAsset::Base,
            total_base_debt_nad,
            total_base_debt_nad,
            self.debt.global_health_quote_contribution_for_base_debt,
            risk,
        )?;
        let (effective_quote_debt_nad, quote_debt_health_bps) = self.global_side_health(
            MarketAsset::Quote,
            total_quote_debt_nad,
            total_quote_debt_nad,
            self.debt.global_health_base_contribution_for_quote_debt,
            risk,
        )?;

        Ok(MarketHealth {
            global_health_base_contribution_for_quote_debt: self.debt.global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: self.debt.global_health_quote_contribution_for_base_debt,
            effective_base_debt_nad,
            effective_quote_debt_nad,
            base_debt_health_bps,
            quote_debt_health_bps,
        })
    }

    pub fn current_risk(&self) -> Result<Risk> {
        let current_slot = Clock::get().map(|clock| clock.slot).unwrap_or(self.last_update_slot);
        let evaluation = self.evaluate_current_curve(current_slot)?;
        self.risk_from_curve_evaluation(evaluation, current_slot)
    }

    fn risk_from_curve_evaluation(&self, evaluation: ConcentratedEvaluation, current_slot: u64) -> Result<Risk> {
        let current_base_price_nad =
            u64::try_from(evaluation.marginal_price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(current_base_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        let current_quote_price_nad = (NAD as u128)
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(current_base_price_nad as u128))
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.risk.refreshed(
            current_base_price_nad,
            current_quote_price_nad,
            evaluation.balanced_equivalent_q,
            &self.config,
            current_slot,
        )
    }

    /// Advances the scalar risk observation to the supplied final curve mark
    /// without claiming that the four pessimistic lending shapes were
    /// materialized. Spot swaps use this after their final reserve endpoint so
    /// later risk operations integrate the post-trade mark over elapsed time.
    /// Exact risk consumers still rebuild the curve identity before use.
    pub(crate) fn observe_risk_from_curve_evaluation(
        &mut self,
        evaluation: ConcentratedEvaluation,
        current_slot: u64,
    ) -> Result<()> {
        let risk = self.risk_from_curve_evaluation(evaluation, current_slot)?;
        self.amm.commit_invariant(evaluation.invariant_d)?;
        self.risk = risk;
        self.last_marginal_observation_nad =
            u64::try_from(evaluation.marginal_price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        self.last_update_slot = current_slot;
        Ok(())
    }

    /// Marks the scalar observation as exact for the current curve revision.
    /// Liquidity and explicit risk-refresh paths use this; ordinary swaps
    /// intentionally leave `risk_revision` behind until a risk-sensitive
    /// operation materializes current risk.
    pub(crate) fn observe_exact_risk_from_curve_evaluation(
        &mut self,
        evaluation: ConcentratedEvaluation,
        current_slot: u64,
    ) -> Result<()> {
        self.observe_risk_from_curve_evaluation(evaluation, current_slot)?;
        self.risk_revision = self.curve_revision;
        Ok(())
    }

    /// Observes exact current price and Q without persisting pessimistic
    /// reserve shapes. Risk consumers reconstruct the applied shape from this
    /// scalar snapshot and the current curve parameters.
    pub(crate) fn observe_current_risk(&mut self, current_slot: u64) -> Result<()> {
        let evaluation = self.evaluate_current_curve(current_slot)?;
        self.observe_exact_risk_from_curve_evaluation(evaluation, current_slot)
    }

    /// Reuses a quote-time endpoint only while every executable curve identity
    /// field still matches. A funded ramp, recenter, retained-reserve mismatch,
    /// or hLP inventory change returns `false` so the caller can evaluate the
    /// actual final state instead.
    #[cfg(test)]
    pub(crate) fn try_observe_risk_from_curve_checkpoint(
        &mut self,
        checkpoint: CurveCheckpoint,
        current_slot: u64,
    ) -> Result<bool> {
        let Some(evaluation) = checkpoint.evaluation_if_matches(self, current_slot)? else {
            return Ok(false);
        };
        self.observe_exact_risk_from_curve_evaluation(evaluation, current_slot)?;
        Ok(true)
    }

    pub fn refresh_risk(&mut self) -> Result<()> {
        let current_slot = Clock::get().map(|clock| clock.slot).unwrap_or(self.last_update_slot);
        self.refresh_risk_at_slot(current_slot)
    }

    /// Materializes risk at an already-read slot. Governance uses this before
    /// changing time constants so the entire elapsed interval is integrated
    /// under the configuration that was active during that interval.
    pub(crate) fn refresh_risk_at_slot(&mut self, current_slot: u64) -> Result<()> {
        let curve_reserves = self.curve_reserves_nad()?;
        if curve_reserves.base == 0 || curve_reserves.quote == 0 {
            require!(
                curve_reserves.base == 0 && curve_reserves.quote == 0,
                ErrorCode::InsufficientLiquidity
            );
            self.risk.cached_q_nad = 0;
            self.risk.last_snapshot_slot = current_slot;
            self.amm.clear_invariant();
            self.last_marginal_observation_nad = 0;
            self.risk_revision = self.curve_revision;
            self.last_update_slot = current_slot;
            return Ok(());
        }

        let evaluation = self.evaluate_current_curve(current_slot)?;
        let risk = self.risk_from_curve_evaluation(evaluation, current_slot)?;
        self.amm.commit_invariant(evaluation.invariant_d)?;
        self.risk = risk;
        self.last_marginal_observation_nad =
            u64::try_from(evaluation.marginal_price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        self.risk_revision = self.curve_revision;
        self.last_update_slot = current_slot;
        Ok(())
    }

    pub fn effective_base_debt_nad(&self) -> Result<u128> {
        Ok(self.market_health()?.effective_base_debt_nad)
    }

    pub fn effective_quote_debt_nad(&self) -> Result<u128> {
        Ok(self.market_health()?.effective_quote_debt_nad)
    }

    fn risk_curve_request(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        include_directional_ema: bool,
    ) -> Result<RiskCurveRequest> {
        let parameters = self.current_curve_parameters(risk.last_snapshot_slot);
        let center_price_nad = self.current_curve_center_price_nad()?;
        let collateral_price_nad =
            self.pessimistic_collateral_price_nad(collateral_asset, risk, include_directional_ema);
        require!(collateral_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        let target_base_price_nad = match collateral_asset {
            MarketAsset::Base => collateral_price_nad as u128,
            MarketAsset::Quote => ceil_div(
                (NAD as u128)
                    .checked_mul(NAD as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                collateral_price_nad as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?,
        };
        Ok(RiskCurveRequest {
            target_base_price_nad,
            balanced_equivalent_q_nad: risk.conservative_q_nad(),
            collateral_to_debt: match collateral_asset {
                MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
            },
            center_price_nad: center_price_nad as u128,
            peak_depth_nad: parameters.peak_depth_nad as u128,
            fade_scale_nad: parameters.fade_scale_nad as u128,
        })
    }

    pub(crate) fn pessimistic_risk_curve(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        include_directional_ema: bool,
    ) -> Result<(ConcentratedRiskCurve, ConcentratedSwapDirection)> {
        let request = self.risk_curve_request(collateral_asset, risk, include_directional_ema)?;
        let reserves = concentrated_risk_reserves_at_price_q(
            request.target_base_price_nad,
            request.balanced_equivalent_q_nad,
            request.collateral_to_debt,
            request.center_price_nad,
            request.peak_depth_nad,
            request.fade_scale_nad,
        )?;
        let parameters = self.current_curve_parameters(risk.last_snapshot_slot);
        Ok((
            ConcentratedRiskCurve {
                base_reserve_nad: reserves.base_reserve_nad,
                quote_reserve_nad: reserves.quote_reserve_nad,
                center_price_nad: self.current_curve_center_price_nad()? as u128,
                peak_depth_nad: parameters.peak_depth_nad as u128,
                fade_scale_nad: parameters.fade_scale_nad as u128,
            },
            match collateral_asset {
                MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
            },
        ))
    }

    pub(crate) fn risk_curve_from_ordered_reserves(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        collateral_reserve_nad: u128,
        debt_reserve_nad: u128,
    ) -> Result<(ConcentratedRiskCurve, ConcentratedSwapDirection)> {
        let parameters = self.current_curve_parameters(risk.last_snapshot_slot);
        let (base_reserve_nad, quote_reserve_nad, direction) = match collateral_asset {
            MarketAsset::Base => (
                collateral_reserve_nad,
                debt_reserve_nad,
                ConcentratedSwapDirection::BaseToQuote,
            ),
            MarketAsset::Quote => (
                debt_reserve_nad,
                collateral_reserve_nad,
                ConcentratedSwapDirection::QuoteToBase,
            ),
        };
        Ok((
            ConcentratedRiskCurve {
                base_reserve_nad,
                quote_reserve_nad,
                center_price_nad: self.current_curve_center_price_nad()? as u128,
                peak_depth_nad: parameters.peak_depth_nad as u128,
                fade_scale_nad: parameters.fade_scale_nad as u128,
            },
            direction,
        ))
    }

    pub(crate) fn total_fixed_debt_nad(&self, debt_asset: MarketAsset) -> Result<u128> {
        let (fixed_debt, debt_decimals) = match debt_asset {
            MarketAsset::Base => (self.debt.fixed_base_debt()?, self.base_side.asset_decimals),
            MarketAsset::Quote => (self.debt.fixed_quote_debt()?, self.quote_side.asset_decimals),
        };
        normalize_to_nad(fixed_debt, debt_decimals)
    }

    pub(crate) fn external_fixed_debt_nad(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
    ) -> Result<u128> {
        let (aggregate_shares, position_shares, borrow_index_nad, debt_decimals) = match debt_asset {
            MarketAsset::Base => (
                self.debt.fixed_base_shares,
                borrow_position.fixed_base_shares,
                self.debt.base_borrow_index_nad,
                self.base_side.asset_decimals,
            ),
            MarketAsset::Quote => (
                self.debt.fixed_quote_shares,
                borrow_position.fixed_quote_shares,
                self.debt.quote_borrow_index_nad,
                self.quote_side.asset_decimals,
            ),
        };
        let external_shares = aggregate_shares
            .checked_sub(position_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        normalize_to_nad(Debt::shares_to_debt(external_shares, borrow_index_nad)?, debt_decimals)
    }

    pub(crate) fn dynamic_borrow_terms(
        &self,
        debt_asset: MarketAsset,
        collateral_amount: u64,
        existing_total_debt_nad: u128,
        projected_total_debt_nad: u128,
        projected_aggregate_contribution: u64,
        risk: &Risk,
    ) -> Result<DynamicBorrowTerms> {
        let collateral_asset = debt_asset.opposite();
        let (effective_existing_debt_nad, projected_market_health_bps) = self.global_side_health(
            debt_asset,
            existing_total_debt_nad,
            projected_total_debt_nad,
            projected_aggregate_contribution,
            risk,
        )?;
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let (curve, collateral_to_debt) = self.pessimistic_risk_curve(collateral_asset, risk, true)?;
        let terms = pessimistic_max_debt_on_curve_nad(
            collateral_amount_nad,
            curve,
            collateral_to_debt,
            effective_existing_debt_nad,
        )?;

        Ok(DynamicBorrowTerms {
            max_debt: denormalize_from_nad_floor(terms.max_debt_nad, self.side(debt_asset).asset_decimals)?,
            max_cf_bps: terms.max_cf_bps,
            liquidation_cf_bps: terms.liquidation_cf_bps,
            effective_existing_debt_nad,
            projected_market_health_bps,
        })
    }

    /// Global health is an underwriting input, not collateral ownership. Each
    /// position contributes at most a linear collateral value equal to the
    /// configured multiple of its own debt.
    pub(crate) fn debt_capped_global_health_contribution(
        &self,
        debt_asset: MarketAsset,
        projected_debt: u128,
        total_collateral: u64,
        risk: &Risk,
    ) -> Result<u64> {
        if projected_debt == 0 || total_collateral == 0 {
            return Ok(0);
        }
        let collateral_asset = debt_asset.opposite();
        let debt_nad = normalize_to_nad(projected_debt, self.side(debt_asset).asset_decimals)?;
        let value_cap_nad = debt_nad
            .checked_mul(self.config.global_health_contribution_cap_bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let price_nad = self.pessimistic_collateral_price_nad(collateral_asset, risk, true) as u128;
        if price_nad == 0 {
            return Ok(0);
        }
        let collateral_cap_nad = value_cap_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(price_nad))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let collateral_cap =
            denormalize_from_nad_floor(collateral_cap_nad, self.side(collateral_asset).asset_decimals)?;
        Ok(total_collateral.min(collateral_cap))
    }

    pub(crate) fn is_position_liquidatable_with_risk(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        risk: &Risk,
    ) -> Result<bool> {
        let debt_nad = normalize_to_nad(
            match debt_asset {
                MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
                MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
            },
            self.side(debt_asset).asset_decimals,
        )?;
        if debt_nad == 0 {
            return Ok(false);
        }
        let liquidation_cf_bps = borrow_position.liquidation_cf_bps(debt_asset);
        if liquidation_cf_bps == 0 {
            return Ok(true);
        }
        let collateral_asset = debt_asset.opposite();
        let collateral_value_nad = self.liquidation_collateral_value_nad(
            collateral_asset,
            borrow_position.collateral(collateral_asset),
            risk,
        )?;
        Ok(debt_nad.saturating_mul(BPS_DENOMINATOR as u128)
            >= collateral_value_nad.saturating_mul(liquidation_cf_bps as u128))
    }

    pub fn is_position_liquidatable(&self, borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> Result<bool> {
        self.is_position_liquidatable_with_risk(borrow_position, debt_asset, &self.current_risk()?)
    }

    pub fn reconcile_liquidation_auction(&self, borrow_position: &mut BorrowPosition) -> Result<()> {
        let Some(debt_asset) = borrow_position.active_liquidation_auction_asset()? else {
            return Ok(());
        };
        if !self.is_position_liquidatable(borrow_position, debt_asset)? {
            borrow_position.clear_liquidation_auction();
        }
        Ok(())
    }

    pub fn assert_market_health(&self) -> Result<()> {
        self.assert_market_health_snapshot(&self.market_health()?)
    }

    pub fn assert_market_health_snapshot(&self, health: &MarketHealth) -> Result<()> {
        if self.debt.fixed_base_shares > 0 {
            require_gte!(
                health.base_debt_health_bps,
                self.config.borrow_market_health_floor_bps as u64,
                ErrorCode::InsufficientMarketHealth
            );
        }
        if self.debt.fixed_quote_shares > 0 {
            require_gte!(
                health.quote_debt_health_bps,
                self.config.borrow_market_health_floor_bps as u64,
                ErrorCode::InsufficientMarketHealth
            );
        }
        Ok(())
    }

    pub(crate) fn collateral_value_nad(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let (curve, collateral_to_debt) = self.pessimistic_risk_curve(collateral_asset, risk, true)?;
        curve.exact_in(collateral_amount_nad, collateral_to_debt)
    }

    pub(crate) fn liquidation_collateral_value_nad(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let (curve, collateral_to_debt) = self.pessimistic_risk_curve(collateral_asset, risk, false)?;
        curve.exact_in(collateral_amount_nad, collateral_to_debt)
    }

    fn global_side_health(
        &self,
        debt_asset: MarketAsset,
        existing_total_debt_nad: u128,
        projected_total_debt_nad: u128,
        aggregate_contribution: u64,
        risk: &Risk,
    ) -> Result<(u128, u64)> {
        if projected_total_debt_nad == 0 {
            return Ok((0, u64::MAX));
        }
        let collateral_asset = debt_asset.opposite();
        let (collateral_reserve_nad, debt_reserve_nad) =
            self.pessimistic_virtual_reserves_nad(collateral_asset, risk, true)?;
        self.global_side_health_with_virtual_reserves(
            debt_asset,
            existing_total_debt_nad,
            projected_total_debt_nad,
            aggregate_contribution,
            risk,
            collateral_reserve_nad,
            debt_reserve_nad,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn global_side_health_with_virtual_reserves(
        &self,
        debt_asset: MarketAsset,
        existing_total_debt_nad: u128,
        projected_total_debt_nad: u128,
        aggregate_contribution: u64,
        risk: &Risk,
        collateral_reserve_nad: u128,
        debt_reserve_nad: u128,
    ) -> Result<(u128, u64)> {
        if projected_total_debt_nad == 0 {
            return Ok((0, u64::MAX));
        }
        let collateral_asset = debt_asset.opposite();
        if projected_total_debt_nad >= debt_reserve_nad {
            return Ok((existing_total_debt_nad, 0));
        }
        let (curve, collateral_to_debt) =
            self.risk_curve_from_ordered_reserves(collateral_asset, risk, collateral_reserve_nad, debt_reserve_nad)?;
        let required_collateral_nad = curve.exact_out(projected_total_debt_nad, collateral_to_debt)?;
        let stored_contribution_nad = normalize_to_nad(
            aggregate_contribution as u128,
            self.side(collateral_asset).asset_decimals,
        )?;
        let contribution_value_cap_nad = projected_total_debt_nad
            .checked_mul(self.config.global_health_contribution_cap_bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let collateral_price_nad = self.pessimistic_collateral_price_nad(collateral_asset, risk, true) as u128;
        let current_contribution_cap_nad = if collateral_price_nad == 0 {
            0
        } else {
            contribution_value_cap_nad
                .checked_mul(NAD as u128)
                .and_then(|value| value.checked_div(collateral_price_nad))
                .ok_or(ErrorCode::MarketMathOverflow)?
        };
        // A contribution is capped both when it is recorded and when it is
        // consumed, so collateral appreciation cannot stale the 150% bound.
        let contribution_nad = stored_contribution_nad.min(current_contribution_cap_nad);
        if contribution_nad == 0 {
            return Ok((existing_total_debt_nad, 0));
        }
        let market_health_bps = u64::try_from(
            contribution_nad
                .checked_mul(BPS_DENOMINATOR as u128)
                .and_then(|value| value.checked_div(required_collateral_nad))
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .unwrap_or(u64::MAX);
        let effective_existing_debt_nad = if required_collateral_nad >= contribution_nad {
            existing_total_debt_nad
        } else {
            ceil_div(
                existing_total_debt_nad
                    .checked_mul(required_collateral_nad)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                contribution_nad,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?
        };
        Ok((effective_existing_debt_nad, market_health_bps))
    }

    pub(crate) fn pessimistic_virtual_reserves_nad(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        include_directional_ema: bool,
    ) -> Result<(u128, u128)> {
        let request = self.risk_curve_request(collateral_asset, risk, include_directional_ema)?;
        let reserves = concentrated_risk_reserves_at_price_q(
            request.target_base_price_nad,
            request.balanced_equivalent_q_nad,
            request.collateral_to_debt,
            request.center_price_nad,
            request.peak_depth_nad,
            request.fade_scale_nad,
        )?;
        Ok(match collateral_asset {
            MarketAsset::Base => (reserves.base_reserve_nad, reserves.quote_reserve_nad),
            MarketAsset::Quote => (reserves.quote_reserve_nad, reserves.base_reserve_nad),
        })
    }

    fn pessimistic_collateral_price_nad(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        include_directional_ema: bool,
    ) -> u64 {
        let (symmetric, directional) = match collateral_asset {
            MarketAsset::Base => (risk.base_price_ema_nad, risk.directional_base_price_ema_nad),
            MarketAsset::Quote => (risk.quote_price_ema_nad, risk.directional_quote_price_ema_nad),
        };
        if include_directional_ema {
            symmetric.min(directional)
        } else {
            symmetric
        }
    }

    pub(crate) fn conservative_risk_reserve_depths(&self, risk: &Risk) -> Result<(u64, u64)> {
        let curve_reserves = self.curve_reserves_nad()?;
        let current_q_nad = if risk.cached_q_nad > 0 {
            risk.cached_q_nad
        } else {
            self.evaluate_current_curve(risk.last_snapshot_slot)?
                .balanced_equivalent_q
        };
        let conservative_q_nad = risk.conservative_q_nad();
        let depths = if conservative_q_nad > 0 && conservative_q_nad < current_q_nad {
            require!(current_q_nad > 0, ErrorCode::InvalidArgument);
            let scaled = ConcentratedRiskReserves {
                base_reserve_nad: mul_div_u128(curve_reserves.base, conservative_q_nad, current_q_nad)?,
                quote_reserve_nad: mul_div_u128(curve_reserves.quote, conservative_q_nad, current_q_nad)?,
            };
            require!(
                scaled.base_reserve_nad > 0 && scaled.quote_reserve_nad > 0,
                ErrorCode::InsufficientLiquidity
            );
            scaled
        } else {
            ConcentratedRiskReserves {
                base_reserve_nad: curve_reserves.base,
                quote_reserve_nad: curve_reserves.quote,
            }
        };
        let base_curve_reserve = self.curve_reserve(MarketAsset::Base)?;
        let quote_curve_reserve = self.curve_reserve(MarketAsset::Quote)?;
        Ok((
            denormalize_from_nad_floor(depths.base_reserve_nad, self.base_side.asset_decimals)?.min(base_curve_reserve),
            denormalize_from_nad_floor(depths.quote_reserve_nad, self.quote_side.asset_decimals)?
                .min(quote_curve_reserve),
        ))
    }

    pub(crate) fn daily_limit_for_side(&self, market_asset: MarketAsset, limit_bps: u16) -> Result<u64> {
        let (base_depth, quote_depth) = self.conservative_risk_reserve_depths(&self.risk)?;
        let depth = match market_asset {
            MarketAsset::Base => base_depth,
            MarketAsset::Quote => quote_depth,
        };
        u64::try_from(
            (depth as u128)
                .checked_mul(limit_bps as u128)
                .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow.into())
    }
}

pub(crate) fn max_cf_bps_from_liquidation_cf(liquidation_cf_bps: u16) -> u16 {
    ((liquidation_cf_bps as u32).saturating_mul((BPS_DENOMINATOR - LTV_BUFFER_BPS) as u32) / BPS_DENOMINATOR as u32)
        as u16
}

pub(crate) fn liquidation_health_floor_bps(liquidation_cf_bps: u16) -> u64 {
    if liquidation_cf_bps == 0 {
        return u64::MAX;
    }
    ceil_div((BPS_DENOMINATOR as u128).pow(2), liquidation_cf_bps as u128)
        .unwrap_or(u128::from(u64::MAX))
        .min(u128::from(u64::MAX)) as u64
}
