mod debt;
mod liquidation;
mod preview;

pub use debt::*;
pub use liquidation::*;
#[cfg(test)]
pub(crate) use preview::NewPositionPreviewContext;

use anchor_lang::prelude::*;

use crate::{
    constants::*,
    errors::ErrorCode,
    math::*,
    state::{BorrowPosition, CollateralReceipt, DailyBorrowBucket, Debt, Market, MarketAsset, Risk},
    transitions::amm::{ConcentratedCurveDirection, ConcentratedCurveGeometry, ConcentratedCurvePoint},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynamicBorrowTerms {
    pub max_debt: u64,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
    pub effective_existing_debt_nad: u128,
    pub projected_market_health_bps: u64,
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
        let price_nad = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let curve_depth_nad = self
            .amm
            .concentrated_curve_cache
            .tail_liquidity
            .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let quote_price_nad = u64::try_from(
            (NAD as u128)
                .checked_mul(NAD as u128)
                .and_then(|value| value.checked_div(price_nad as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        self.risk
            .refreshed(price_nad, quote_price_nad, curve_depth_nad, &self.config, current_slot)
    }

    /// O(1) risk observation for the concentrated tail+band curve. The quote has
    /// already evaluated the exact final marginal price; total concentrated
    /// liquidity replaces the legacy balanced-equivalent root.
    pub(crate) fn observe_risk_from_concentrated_curve(
        &mut self,
        current_base_price_nad: u64,
        current_curve_depth_nad: u128,
        current_slot: u64,
    ) -> Result<()> {
        require!(
            current_base_price_nad > 0 && current_curve_depth_nad > 0,
            ErrorCode::InsufficientLiquidity
        );
        let current_quote_price_nad = (NAD as u128)
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(current_base_price_nad as u128))
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.risk = self.risk.refreshed(
            current_base_price_nad,
            current_quote_price_nad,
            current_curve_depth_nad,
            &self.config,
            current_slot,
        )?;
        self.last_marginal_observation_nad = current_base_price_nad;
        self.last_update_slot = current_slot;
        Ok(())
    }

    /// Observes exact current price and curve depth without persisting pessimistic
    /// reserve shapes. Risk consumers reconstruct the applied shape from this
    /// scalar snapshot and the current curve parameters.
    pub(crate) fn observe_current_risk(&mut self, current_slot: u64) -> Result<()> {
        let price_nad = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let curve_depth_nad = self
            .amm
            .concentrated_curve_cache
            .tail_liquidity
            .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        self.observe_risk_from_concentrated_curve(price_nad, curve_depth_nad, current_slot)?;
        self.risk_revision = self.curve_revision;
        Ok(())
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
            self.risk.observed_curve_depth_nad = 0;
            self.risk.last_snapshot_slot = current_slot;
            self.amm.concentrated_curve_cache = Default::default();
            self.last_marginal_observation_nad = 0;
            self.risk_revision = self.curve_revision;
            self.last_update_slot = current_slot;
            return Ok(());
        }

        let price_nad = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let curve_depth_nad = self
            .amm
            .concentrated_curve_cache
            .tail_liquidity
            .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        self.observe_risk_from_concentrated_curve(price_nad, curve_depth_nad, current_slot)?;
        self.risk_revision = self.curve_revision;
        Ok(())
    }

    fn pessimistic_curve_price_and_direction(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        include_directional_ema: bool,
    ) -> Result<(u128, ConcentratedCurveDirection)> {
        let collateral_price_nad =
            self.pessimistic_collateral_price_nad(collateral_asset, risk, include_directional_ema);
        require!(collateral_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        let base_price_nad = match collateral_asset {
            MarketAsset::Base => collateral_price_nad as u128,
            MarketAsset::Quote => ceil_div(
                (NAD as u128)
                    .checked_mul(NAD as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                collateral_price_nad as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?,
        };
        let direction = match collateral_asset {
            MarketAsset::Base => ConcentratedCurveDirection::BaseToQuote,
            MarketAsset::Quote => ConcentratedCurveDirection::QuoteToBase,
        };
        Ok((base_price_nad, direction))
    }

    /// Rebuilds the full concentrated curve at the pessimistic price and
    /// depth. This is used to price the external-liquidation auction floor.
    pub(crate) fn pessimistic_concentrated_curve(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        include_directional_ema: bool,
    ) -> Result<
        Option<(
            ConcentratedCurveGeometry,
            ConcentratedCurvePoint,
            ConcentratedCurveDirection,
        )>,
    > {
        let (base_price_nad, direction) =
            self.pessimistic_curve_price_and_direction(collateral_asset, risk, include_directional_ema)?;
        let mut cache = self.amm.concentrated_curve_cache;
        let current_total_liquidity = cache
            .tail_liquidity
            .checked_add(cache.concentrated_liquidity)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(current_total_liquidity > 0, ErrorCode::InsufficientLiquidity);
        let pessimistic_depth_nad = risk.pessimistic_depth_nad();
        let target_total_liquidity = if pessimistic_depth_nad == 0 {
            current_total_liquidity
        } else {
            pessimistic_depth_nad.min(current_total_liquidity)
        };
        cache.tail_liquidity = mul_div_u128(cache.tail_liquidity, target_total_liquidity, current_total_liquidity)?;
        cache.concentrated_liquidity = mul_div_u128(
            cache.concentrated_liquidity,
            target_total_liquidity,
            current_total_liquidity,
        )?;
        require!(cache.tail_liquidity > 0, ErrorCode::InsufficientLiquidity);
        if self.amm.concentrated_curve_cache.concentrated_liquidity > 0 {
            require!(cache.concentrated_liquidity > 0, ErrorCode::InsufficientLiquidity);
        }
        if cache.fade_width_bps > 0 {
            require!(cache.concentrated_liquidity >= 2, ErrorCode::InsufficientLiquidity);
        }
        let geometry = cache.geometry()?;
        let point = geometry.point_at_price_nad(base_price_nad, cache.tail_liquidity)?;
        Ok(Some((geometry, point, direction)))
    }

    /// Pessimistic borrowing shadow curve. Only the full-range tail can
    /// underwrite debt; concentrated liquidity is excluded.
    fn pessimistic_borrow_cpmm(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        include_directional_ema: bool,
    ) -> Result<
        Option<(
            ConcentratedCurveGeometry,
            ConcentratedCurvePoint,
            ConcentratedCurveDirection,
        )>,
    > {
        let (base_price_nad, direction) =
            self.pessimistic_curve_price_and_direction(collateral_asset, risk, include_directional_ema)?;
        let cache = self.amm.concentrated_curve_cache;
        let current_total_liquidity = cache
            .tail_liquidity
            .checked_add(cache.concentrated_liquidity)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(current_total_liquidity > 0, ErrorCode::InsufficientLiquidity);
        let pessimistic_depth_nad = risk.pessimistic_depth_nad();
        let target_total_liquidity = if pessimistic_depth_nad == 0 {
            current_total_liquidity
        } else {
            pessimistic_depth_nad.min(current_total_liquidity)
        };
        let target_tail_liquidity =
            mul_div_u128(cache.tail_liquidity, target_total_liquidity, current_total_liquidity)?;
        require!(target_tail_liquidity > 0, ErrorCode::InsufficientLiquidity);
        let geometry = ConcentratedCurveGeometry::cpmm();
        let point = geometry.point_at_price_nad(base_price_nad, target_tail_liquidity)?;
        Ok(Some((geometry, point, direction)))
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
        let (geometry, point, direction) = self
            .pessimistic_borrow_cpmm(collateral_asset, risk, true)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let impact_value = geometry
            .quote_exact_in(point, collateral_amount_nad, direction)?
            .amount_out;
        let utilized = if effective_existing_debt_nad == 0 {
            0
        } else {
            geometry
                .quote_exact_out(point, effective_existing_debt_nad, direction)?
                .amount_in
        };
        let total_value = geometry
            .quote_exact_in(
                point,
                utilized
                    .checked_add(collateral_amount_nad)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                direction,
            )?
            .amount_out;
        let user_max_debt = total_value.saturating_sub(effective_existing_debt_nad);
        let base_cf_bps = if impact_value == 0 {
            0
        } else {
            user_max_debt
                .saturating_mul(BPS_DENOMINATOR as u128)
                .checked_div(impact_value)
                .unwrap_or(0)
        };
        let liquidation_cf_bps = base_cf_bps.min(MAX_COLLATERAL_FACTOR_BPS as u128) as u16;
        let max_cf_bps = ((liquidation_cf_bps as u32).saturating_mul((BPS_DENOMINATOR - LTV_BUFFER_BPS) as u32)
            / BPS_DENOMINATOR as u32) as u16;
        let terms = DynamicCollateralTerms {
            max_debt_nad: impact_value
                .saturating_mul(max_cf_bps as u128)
                .checked_div(BPS_DENOMINATOR as u128)
                .unwrap_or(0),
            max_cf_bps,
            liquidation_cf_bps,
        };

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
        let collateral_value_nad = self.linear_liquidation_collateral_value_nad(
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
        if collateral_amount == 0 {
            return Ok(0);
        }
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let (geometry, point, direction) = self
            .pessimistic_borrow_cpmm(collateral_asset, risk, true)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        geometry
            .quote_exact_in(point, collateral_amount_nad, direction)
            .map(|quote| quote.amount_out)
    }

    #[cfg(test)]
    pub(crate) fn liquidation_collateral_value_nad(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        if collateral_amount == 0 {
            return Ok(0);
        }
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let (geometry, point, direction) = self
            .pessimistic_concentrated_curve(collateral_asset, risk, false)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        geometry
            .quote_exact_in(point, collateral_amount_nad, direction)
            .map(|quote| quote.amount_out)
    }

    /// Liquidatability is a linear collateral test at the symmetric EMA.
    /// Depth and trade slippage belong to execution pricing, not the trigger.
    pub(crate) fn linear_liquidation_collateral_value_nad(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let price_nad = self.pessimistic_collateral_price_nad(collateral_asset, risk, false);
        require!(price_nad > 0, ErrorCode::InvalidSettlementPrice);
        collateral_amount_nad
            .checked_mul(price_nad as u128)
            .and_then(|value| value.checked_div(NAD as u128))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
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
        let geometry = ConcentratedCurveGeometry::cpmm();
        let (point, direction) = match collateral_asset {
            MarketAsset::Base => (
                ConcentratedCurvePoint {
                    base_reserve: collateral_reserve_nad,
                    quote_reserve: debt_reserve_nad,
                },
                ConcentratedCurveDirection::BaseToQuote,
            ),
            MarketAsset::Quote => (
                ConcentratedCurvePoint {
                    base_reserve: debt_reserve_nad,
                    quote_reserve: collateral_reserve_nad,
                },
                ConcentratedCurveDirection::QuoteToBase,
            ),
        };
        let required_collateral_nad = geometry
            .quote_exact_out(point, projected_total_debt_nad, direction)?
            .amount_in;
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
        let (_, point, _) = self
            .pessimistic_borrow_cpmm(collateral_asset, risk, include_directional_ema)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        Ok(match collateral_asset {
            MarketAsset::Base => (point.base_reserve, point.quote_reserve),
            MarketAsset::Quote => (point.quote_reserve, point.base_reserve),
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

    pub(crate) fn pessimistic_borrow_reserve_depths(&self, risk: &Risk) -> Result<(u64, u64)> {
        let (base_nad, quote_nad) = self.pessimistic_virtual_reserves_nad(MarketAsset::Base, risk, true)?;
        Ok((
            denormalize_from_nad_floor(base_nad, self.base_side.asset_decimals)?
                .min(self.curve_reserve(MarketAsset::Base)?),
            denormalize_from_nad_floor(quote_nad, self.quote_side.asset_decimals)?
                .min(self.curve_reserve(MarketAsset::Quote)?),
        ))
    }

    pub(crate) fn daily_limit_for_side(&self, market_asset: MarketAsset, limit_bps: u16) -> Result<u64> {
        let (base_depth, quote_depth) = self.pessimistic_borrow_reserve_depths(&self.risk)?;
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

impl DailyBorrowBucket {
    pub fn decay_to_slot(&mut self, limit: u64, current_slot: u64) -> Result<()> {
        let elapsed_ms = slots_to_ms(self.last_decay_slot, current_slot).ok_or(ErrorCode::InvalidArgument)?;
        if self.borrowed_bucket == 0 {
            self.decay_remainder_ms = 0;
        } else if elapsed_ms > 0 {
            let released_numerator = (limit as u128)
                .checked_mul(elapsed_ms as u128)
                .and_then(|value| value.checked_add(self.decay_remainder_ms as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let released = released_numerator / MS_PER_DAY as u128;
            if released >= self.borrowed_bucket as u128 {
                self.borrowed_bucket = 0;
                self.decay_remainder_ms = 0;
            } else {
                let released = u64::try_from(released).map_err(|_| ErrorCode::MarketMathOverflow)?;
                self.borrowed_bucket = self
                    .borrowed_bucket
                    .checked_sub(released)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.decay_remainder_ms = u64::try_from(released_numerator % MS_PER_DAY as u128)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?;
            }
        }
        self.last_decay_slot = current_slot;
        Ok(())
    }

    pub fn record_borrow(&mut self, amount: u64, limit: u64, current_slot: u64) -> Result<()> {
        self.decay_to_slot(limit, current_slot)?;
        let next_bucket = self
            .borrowed_bucket
            .checked_add(amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(limit, next_bucket, ErrorCode::DailyLimitExceeded);
        self.borrowed_bucket = next_bucket;
        Ok(())
    }

    pub fn remaining(&self, limit: u64, current_slot: u64) -> Result<u64> {
        let mut decayed = *self;
        decayed.decay_to_slot(limit, current_slot)?;
        Ok(limit.saturating_sub(decayed.borrowed_bucket))
    }
}

pub(crate) fn accrue_side(market: &mut Market, asset: MarketAsset, current_slot: u64) -> Result<()> {
    let (index, rate_at_target, last_accrual_slot, fixed_shares, isolated_shares) = match asset {
        MarketAsset::Base => (
            market.debt.base_borrow_index_nad,
            market.debt.base_rate_at_target_nad,
            market.debt.base_last_accrual_slot,
            market.debt.fixed_base_shares,
            market.debt.isolated_base_shares,
        ),
        MarketAsset::Quote => (
            market.debt.quote_borrow_index_nad,
            market.debt.quote_rate_at_target_nad,
            market.debt.quote_last_accrual_slot,
            market.debt.fixed_quote_shares,
            market.debt.isolated_quote_shares,
        ),
    };
    if current_slot <= last_accrual_slot {
        return Ok(());
    }
    let dt_ms = current_slot
        .checked_sub(last_accrual_slot)
        .ok_or(ErrorCode::MarketMathOverflow)?
        .saturating_mul(TARGET_MS_PER_SLOT);

    let hlp_shares = match asset {
        MarketAsset::Base => market.quote_hlp_vault.debt_shares,
        MarketAsset::Quote => market.base_hlp_vault.debt_shares,
    };
    if fixed_shares == 0 && isolated_shares == 0 && hlp_shares == 0 {
        let next_rate_at_target = adapt_rate_at_target_nad(
            rate_at_target,
            -(NAD as i128),
            dt_ms,
            market.config.irm.adjustment_speed_per_year as u128,
            INTEREST_MIN_RATE_AT_TARGET_NAD,
            INTEREST_MAX_RATE_AT_TARGET_NAD,
            INTEREST_MAX_ADAPTATION_STEP_NAD,
        )?;
        match asset {
            MarketAsset::Base => {
                market.debt.base_rate_at_target_nad = next_rate_at_target;
                market.debt.base_last_accrual_slot = current_slot;
            }
            MarketAsset::Quote => {
                market.debt.quote_rate_at_target_nad = next_rate_at_target;
                market.debt.quote_last_accrual_slot = current_slot;
            }
        }
        return Ok(());
    }
    let (cash, live) = match asset {
        MarketAsset::Base => (
            market.base_side.reserves.cash_reserve as u128,
            market.base_side.reserves.live_reserve as u128,
        ),
        MarketAsset::Quote => (
            market.quote_side.reserves.cash_reserve as u128,
            market.quote_side.reserves.live_reserve as u128,
        ),
    };
    let hlp_live = market.hlp_live_reserve(asset)?;
    let cash_backed_before = live
        .checked_sub(cash)
        .and_then(|value| value.checked_sub(hlp_live))
        .ok_or(ErrorCode::BrokenInvariant)?;
    if fixed_shares == 0 && isolated_shares == 0 {
        require_eq!(cash_backed_before, 0, ErrorCode::BrokenInvariant);
    }
    let hlp_debt_before = if hlp_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(hlp_shares, index)?
    };

    // Calculate utilization rates. hLP funding debt counts toward funding cost,
    // but only cash-backed debt accrual grows virtual reserves.
    let debt_before = cash_backed_before
        .checked_add(hlp_debt_before)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let util = utilization_bps(debt_before, cash)?;
    let error = utilization_error_nad(util, market.config.irm.target_utilization_bps as u64)?;
    let rate = instantaneous_rate_apr_nad(rate_at_target, error, market.config.irm.curve_steepness_nad as u128)?;
    if hlp_shares > 0 {
        let vault = match asset {
            // Quote hLP borrows Base; Base hLP borrows Quote.
            MarketAsset::Base => &mut market.quote_hlp_vault,
            MarketAsset::Quote => &mut market.base_hlp_vault,
        };
        vault.funding_apr_ema_nad = crate::math::risk::ema_u128_including_zero(
            vault.funding_apr_ema_nad,
            rate,
            vault.funding_apr_ema_last_slot,
            current_slot,
            HLP_FUNDING_APR_EMA_HALF_LIFE_MS,
        );
        vault.funding_apr_ema_last_slot = current_slot;
    }
    let next_index = if index == 0 || dt_ms == 0 || rate == 0 {
        index
    } else {
        let elapsed_ms = dt_ms.min(MAX_INTEREST_ACCRUAL_MS) as u128;
        let growth_nad = rate
            .checked_mul(elapsed_ms)
            .and_then(|value| value.checked_div(MS_PER_YEAR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if growth_nad == 0 {
            index
        } else {
            let delta = index
                .checked_mul(growth_nad)
                .and_then(|value| value.checked_div(NAD as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            index.checked_add(delta).ok_or(ErrorCode::MarketMathOverflow)?
        }
    };
    let next_rate_at_target = adapt_rate_at_target_nad(
        rate_at_target,
        error,
        dt_ms,
        market.config.irm.adjustment_speed_per_year as u128,
        INTEREST_MIN_RATE_AT_TARGET_NAD,
        INTEREST_MAX_RATE_AT_TARGET_NAD,
        INTEREST_MAX_ADAPTATION_STEP_NAD,
    )?;
    // Fixed and isolated buckets remain separately floored. Combined
    // conversion would manufacture an atom at some index boundaries.
    let fixed_after = if fixed_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(fixed_shares, next_index)?
    };
    let isolated_after = if isolated_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(isolated_shares, next_index)?
    };
    let accrued_interest = fixed_after
        .checked_add(isolated_after)
        .and_then(|after| after.checked_sub(cash_backed_before))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if accrued_interest > 0 {
        let accrued_interest = u64::try_from(accrued_interest).map_err(|_| ErrorCode::ReserveOverflow)?;
        let side = market.side_mut(asset);
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_add(accrued_interest)
            .ok_or(ErrorCode::ReserveOverflow)?;
    }

    match asset {
        MarketAsset::Base => {
            market.debt.base_borrow_index_nad = next_index;
            market.debt.base_rate_at_target_nad = next_rate_at_target;
            market.debt.base_last_accrual_slot = current_slot;
        }
        MarketAsset::Quote => {
            market.debt.quote_borrow_index_nad = next_index;
            market.debt.quote_rate_at_target_nad = next_rate_at_target;
            market.debt.quote_last_accrual_slot = current_slot;
        }
    }
    Ok(())
}

pub(crate) fn total_cash_backed_borrowed(market: &Market, asset: MarketAsset, index_nad: u128) -> Result<u128> {
    let (margin_fixed, isolated) = match asset {
        MarketAsset::Base => (market.debt.fixed_base_shares, market.debt.isolated_base_shares),
        MarketAsset::Quote => (market.debt.fixed_quote_shares, market.debt.isolated_quote_shares),
    };
    let margin_fixed_debt = Debt::shares_to_debt(margin_fixed, index_nad)?;
    let isolated_debt = Debt::shares_to_debt(isolated, index_nad)?;
    margin_fixed_debt
        .checked_add(isolated_debt)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

pub(crate) fn reconcile_global_health_contribution(
    position_contribution: &mut u64,
    aggregate_contribution: &mut u64,
    target_contribution: u64,
) -> Result<()> {
    match target_contribution.cmp(position_contribution) {
        std::cmp::Ordering::Greater => {
            let delta = target_contribution
                .checked_sub(*position_contribution)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            *aggregate_contribution = aggregate_contribution
                .checked_add(delta)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        std::cmp::Ordering::Less => {
            let delta = position_contribution
                .checked_sub(target_contribution)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            *aggregate_contribution = aggregate_contribution
                .checked_sub(delta)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        std::cmp::Ordering::Equal => {}
    }

    *position_contribution = target_contribution;
    Ok(())
}

impl Market {
    /// Accrue borrow interest up to the current slot. Should be called before any
    /// debt-dependent computation in an instruction (borrow/repay, hedge,
    /// liquidation, yield claims, swaps, and liquidity changes).
    pub fn accrue_interest(&mut self) -> Result<()> {
        let current_slot = Clock::get()?.slot;
        self.accrue_interest_to_slot(current_slot)
    }

    pub fn update(&mut self) -> Result<()> {
        self.assert_current_version()?;
        let current_slot = Clock::get()?.slot;
        self.accrue_interest_to_slot(current_slot)?;
        if self.base_side.reserves.live_reserve > 0 && self.quote_side.reserves.live_reserve > 0 {
            // hLP exposure is checkpointed from actual state. New hLP entry
            // remains gated while a due concentrated controller target would
            // otherwise price the mint against a stale NAV basis.
            self.advance_amm_clock(current_slot)?;
            self.checkpoint_hlp_vaults()?;
            self.refresh_risk()?;
        }
        Ok(())
    }

    pub(crate) fn accrue_interest_to_slot(&mut self, current_slot: u64) -> Result<()> {
        accrue_side(self, MarketAsset::Base, current_slot)?;
        accrue_side(self, MarketAsset::Quote, current_slot)?;
        Ok(())
    }

    pub fn deposit_collateral(
        &mut self,
        borrow_position: &mut BorrowPosition,
        market_asset: MarketAsset,
        collateral_credit: u64,
    ) -> Result<CollateralReceipt> {
        require!(collateral_credit > 0, ErrorCode::AmountZero);
        let projected_collateral = borrow_position
            .collateral(market_asset)
            .checked_add(collateral_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let debt_asset = market_asset.opposite();
        let projected_debt = match debt_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution =
            self.debt_capped_global_health_contribution(debt_asset, projected_debt, projected_collateral, &self.risk)?;

        match market_asset {
            MarketAsset::Base => borrow_position.base_collateral = projected_collateral,
            MarketAsset::Quote => borrow_position.quote_collateral = projected_collateral,
        }
        self.reconcile_global_health_contribution(borrow_position, debt_asset, target_contribution)?;
        self.reconcile_liquidation_auction(borrow_position)?;

        Ok(CollateralReceipt {
            collateral_credit,
            collateral_debit: 0,
            base_collateral: borrow_position.base_collateral,
            quote_collateral: borrow_position.quote_collateral,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
        })
    }

    pub fn withdraw_collateral(
        &mut self,
        borrow_position: &mut BorrowPosition,
        market_asset: MarketAsset,
        collateral_debit: u64,
        min_liquidation_cf_bps: u16,
    ) -> Result<CollateralReceipt> {
        require!(collateral_debit > 0, ErrorCode::AmountZero);
        let projected_collateral = borrow_position
            .collateral(market_asset)
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientBalance)?;
        let debt_asset = market_asset.opposite();
        let position_debt = match debt_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution =
            self.debt_capped_global_health_contribution(debt_asset, position_debt, projected_collateral, &self.risk)?;

        if position_debt > 0 {
            let total_debt_nad = self.total_fixed_debt_nad(debt_asset)?;
            let external_debt_nad = self.external_fixed_debt_nad(borrow_position, debt_asset)?;
            let projected_aggregate =
                self.projected_aggregate_global_health_contribution(borrow_position, debt_asset, target_contribution)?;
            let terms = self.dynamic_borrow_terms(
                debt_asset,
                projected_collateral,
                external_debt_nad,
                total_debt_nad,
                projected_aggregate,
                &self.risk,
            )?;
            // A third party cannot lower this position's already-issued terms.
            // The owner may withdraw whenever the post-withdraw position remains
            // inside its stored 5% buffered liquidation CF.
            let liquidation_cf_bps = borrow_position
                .liquidation_cf_bps(debt_asset)
                .max(terms.liquidation_cf_bps);
            let collateral_value_nad = self.collateral_value_nad(market_asset, projected_collateral, &self.risk)?;
            let max_debt_nad = collateral_value_nad
                .checked_mul(max_cf_bps_from_liquidation_cf(liquidation_cf_bps) as u128)
                .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let max_debt = denormalize_from_nad_floor(max_debt_nad, self.side(market_asset.opposite()).asset_decimals)?;
            require_gte!(max_debt as u128, position_debt, ErrorCode::InsufficientMarketHealth);
            require_gte!(liquidation_cf_bps, min_liquidation_cf_bps, ErrorCode::SlippageExceeded);
            borrow_position.set_liquidation_cf_bps(debt_asset, liquidation_cf_bps);
        } else {
            borrow_position.set_liquidation_cf_bps(debt_asset, 0);
        }

        match market_asset {
            MarketAsset::Base => borrow_position.base_collateral = projected_collateral,
            MarketAsset::Quote => borrow_position.quote_collateral = projected_collateral,
        }
        self.reconcile_global_health_contribution(borrow_position, debt_asset, target_contribution)?;

        Ok(CollateralReceipt {
            collateral_credit: 0,
            collateral_debit,
            base_collateral: borrow_position.base_collateral,
            quote_collateral: borrow_position.quote_collateral,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
        })
    }

    pub fn borrow(
        &mut self,
        borrow_position: &mut BorrowPosition,
        borrow_asset: MarketAsset,
        borrow_amount: u64,
        min_liquidation_cf_bps: u16,
        current_slot: u64,
    ) -> Result<DebtReceipt> {
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_delta = i64::try_from(borrow_amount).map_err(|_| ErrorCode::Overflow)?;
        if self.risk.curve_depth_ema_nad == 0 {
            self.refresh_risk_at_slot(current_slot)?;
        }
        let risk = self.risk;
        let current_health = self.market_health_from_risk(&risk)?;
        self.assert_market_health_snapshot(&current_health)?;
        // The V1 curve prices debt already issued to other positions. Counting
        // this position's own debt here would make repeated draws worse than
        // opening equivalent split positions.
        let external_debt_nad = self.external_fixed_debt_nad(borrow_position, borrow_asset)?;
        let debt_shares = match borrow_asset {
            MarketAsset::Base => Debt::debt_to_shares(borrow_amount, self.debt.base_borrow_index_nad)?,
            MarketAsset::Quote => Debt::debt_to_shares(borrow_amount, self.debt.quote_borrow_index_nad)?,
        };
        let aggregate_debt_increase = self.debt.fixed_debt_increase_for_shares(borrow_asset, debt_shares)?;
        let (projected_position_debt, projected_total_debt) = match borrow_asset {
            MarketAsset::Base => (
                Debt::shares_to_debt(
                    borrow_position
                        .fixed_base_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.base_borrow_index_nad,
                )?,
                Debt::shares_to_debt(
                    self.debt
                        .fixed_base_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.base_borrow_index_nad,
                )?,
            ),
            MarketAsset::Quote => (
                Debt::shares_to_debt(
                    borrow_position
                        .fixed_quote_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.quote_borrow_index_nad,
                )?,
                Debt::shares_to_debt(
                    self.debt
                        .fixed_quote_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.quote_borrow_index_nad,
                )?,
            ),
        };
        let collateral_asset = borrow_asset.opposite();
        let collateral_amount = borrow_position.collateral(collateral_asset);
        let target_contribution = self.debt_capped_global_health_contribution(
            borrow_asset,
            projected_position_debt,
            collateral_amount,
            &risk,
        )?;
        let projected_aggregate =
            self.projected_aggregate_global_health_contribution(borrow_position, borrow_asset, target_contribution)?;
        let projected_total_debt_nad = normalize_to_nad(projected_total_debt, self.side(borrow_asset).asset_decimals)?;
        let terms = self.dynamic_borrow_terms(
            borrow_asset,
            collateral_amount,
            external_debt_nad,
            projected_total_debt_nad,
            projected_aggregate,
            &risk,
        )?;
        require_gte!(
            terms.max_debt as u128,
            projected_position_debt,
            ErrorCode::InsufficientMarketHealth
        );
        require_gte!(
            terms.liquidation_cf_bps,
            min_liquidation_cf_bps,
            ErrorCode::SlippageExceeded
        );
        require_gte!(
            terms.projected_market_health_bps,
            self.config.borrow_market_health_floor_bps as u64,
            ErrorCode::InsufficientMarketHealth
        );
        require_gte!(
            self.side(borrow_asset).reserves.cash_reserve,
            borrow_amount,
            ErrorCode::InsufficientBorrowHeadroom
        );
        let daily_borrow_limit = self.daily_limit_for_side(borrow_asset, self.config.max_daily_borrow_bps)?;
        self.side_mut(borrow_asset).daily_borrow_bucket.record_borrow(
            borrow_amount,
            daily_borrow_limit,
            current_slot,
        )?;
        let debt_side = self.side_mut(borrow_asset);
        debt_side.reserves.cash_reserve = debt_side
            .reserves
            .cash_reserve
            .checked_sub(borrow_amount)
            .ok_or(ErrorCode::CashReserveUnderflow)?;
        if aggregate_debt_increase > borrow_amount {
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_add(aggregate_debt_increase - borrow_amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
        } else if aggregate_debt_increase < borrow_amount {
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_sub(borrow_amount - aggregate_debt_increase)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }

        match borrow_asset {
            MarketAsset::Base => {
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_base_shares = self
                    .debt
                    .fixed_base_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            MarketAsset::Quote => {
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_quote_shares = self
                    .debt
                    .fixed_quote_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }
        self.debt.add_margin_principal(borrow_asset, borrow_amount)?;
        self.reconcile_global_health_contribution(borrow_position, borrow_asset, target_contribution)?;
        borrow_position.set_liquidation_cf_bps(borrow_asset, terms.liquidation_cf_bps);
        let market_health = self.market_health()?;
        DebtReceipt::from_market(self, borrow_position, debt_delta, 0, 0, &market_health)
    }

    pub(crate) fn projected_aggregate_global_health_contribution(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        target_contribution: u64,
    ) -> Result<u64> {
        let (position_contribution, aggregate_contribution) = match debt_asset {
            MarketAsset::Base => (
                borrow_position.global_health_quote_contribution_for_base_debt,
                self.debt.global_health_quote_contribution_for_base_debt,
            ),
            MarketAsset::Quote => (
                borrow_position.global_health_base_contribution_for_quote_debt,
                self.debt.global_health_base_contribution_for_quote_debt,
            ),
        };
        aggregate_contribution
            .checked_sub(position_contribution)
            .and_then(|value| value.checked_add(target_contribution))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn reconcile_global_health_contribution(
        &mut self,
        borrow_position: &mut BorrowPosition,
        debt_asset: MarketAsset,
        target_contribution: u64,
    ) -> Result<()> {
        match debt_asset {
            MarketAsset::Base => reconcile_global_health_contribution(
                &mut borrow_position.global_health_quote_contribution_for_base_debt,
                &mut self.debt.global_health_quote_contribution_for_base_debt,
                target_contribution,
            ),
            MarketAsset::Quote => reconcile_global_health_contribution(
                &mut borrow_position.global_health_base_contribution_for_quote_debt,
                &mut self.debt.global_health_base_contribution_for_quote_debt,
                target_contribution,
            ),
        }
    }

    pub fn repay(
        &mut self,
        borrow_position: &mut BorrowPosition,
        repay_asset: MarketAsset,
        repay_credit: u64,
    ) -> Result<DebtReceipt> {
        let repayment = self.fixed_repayment_for_max(borrow_position, repay_asset, repay_credit)?;
        // Instruction handlers preview this amount before moving tokens. Keep
        // the state boundary exact so no transferred atom can become an
        // unaccounted donation if state changed unexpectedly.
        require_eq!(repayment.cash_repaid, repay_credit, ErrorCode::BrokenInvariant);
        let (interest_paid, debt_reduction) = match repay_asset {
            MarketAsset::Base => {
                let shares_to_burn = repayment.shares_to_burn;
                let debt_reduction = repayment.position_debt_reduced;
                let aggregate_debt_reduction =
                    self.debt.fixed_debt_reduction_for_shares(repay_asset, shares_to_burn)?;
                let interest_paid =
                    self.debt
                        .realize_margin_liquidation(repay_asset, repay_credit, aggregate_debt_reduction)?;
                let principal_credit = repay_credit
                    .checked_sub(interest_paid)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let live_debit = aggregate_debt_reduction
                    .checked_sub(principal_credit)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_base_shares = self
                    .debt
                    .fixed_base_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.base_side.reserves.live_reserve = self
                    .base_side
                    .reserves
                    .live_reserve
                    .checked_sub(live_debit)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
                self.base_side.reserves.cash_reserve = self
                    .base_side
                    .reserves
                    .cash_reserve
                    .checked_add(principal_credit)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                (interest_paid, debt_reduction)
            }
            MarketAsset::Quote => {
                let shares_to_burn = repayment.shares_to_burn;
                let debt_reduction = repayment.position_debt_reduced;
                let aggregate_debt_reduction =
                    self.debt.fixed_debt_reduction_for_shares(repay_asset, shares_to_burn)?;
                let interest_paid =
                    self.debt
                        .realize_margin_liquidation(repay_asset, repay_credit, aggregate_debt_reduction)?;
                let principal_credit = repay_credit
                    .checked_sub(interest_paid)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let live_debit = aggregate_debt_reduction
                    .checked_sub(principal_credit)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_quote_shares = self
                    .debt
                    .fixed_quote_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.quote_side.reserves.live_reserve = self
                    .quote_side
                    .reserves
                    .live_reserve
                    .checked_sub(live_debit)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
                self.quote_side.reserves.cash_reserve = self
                    .quote_side
                    .reserves
                    .cash_reserve
                    .checked_add(principal_credit)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                (interest_paid, debt_reduction)
            }
        };
        let debt_delta = -i64::try_from(debt_reduction).map_err(|_| ErrorCode::Overflow)?;
        self.refresh_risk()?;
        let debt_after = match repay_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution = self.debt_capped_global_health_contribution(
            repay_asset,
            debt_after,
            borrow_position.collateral(repay_asset.opposite()),
            &self.risk,
        )?;
        self.reconcile_global_health_contribution(borrow_position, repay_asset, target_contribution)?;
        if debt_after == 0 {
            borrow_position.set_liquidation_cf_bps(repay_asset, 0);
            borrow_position.clear_referral_binding(repay_asset);
        }
        self.reconcile_liquidation_auction(borrow_position)?;
        let market_health = self.market_health()?;
        DebtReceipt::from_market(
            self,
            borrow_position,
            debt_delta,
            repayment.cash_repaid,
            interest_paid,
            &market_health,
        )
    }

    pub fn fixed_repayment_for_max(
        &self,
        borrow_position: &BorrowPosition,
        repay_asset: MarketAsset,
        max_repay_amount: u64,
    ) -> Result<DebtRepaymentQuote> {
        let (position_shares, aggregate_shares, borrow_index_nad) = match repay_asset {
            MarketAsset::Base => (
                borrow_position.fixed_base_shares,
                self.debt.fixed_base_shares,
                self.debt.base_borrow_index_nad,
            ),
            MarketAsset::Quote => (
                borrow_position.fixed_quote_shares,
                self.debt.fixed_quote_shares,
                self.debt.quote_borrow_index_nad,
            ),
        };
        Debt::repayment_for_max(position_shares, aggregate_shares, borrow_index_nad, max_repay_amount)
    }

    /// Point-in-time lending utilization used by both the IRM and parameter
    /// execution guard. Funding debt belongs to the side whose token was
    /// borrowed, so it is stored on the opposite hLP aggregate vault.
    pub fn lending_utilization_bps(&self, asset: MarketAsset) -> Result<u64> {
        let fixed_debt = match asset {
            MarketAsset::Base => self.debt.fixed_base_debt()?,
            MarketAsset::Quote => self.debt.fixed_quote_debt()?,
        };
        let isolated_debt = self.debt.isolated_debt(asset)?;
        let hlp_funding_debt = self.hlp_funding_debt(asset)?;
        let total_debt = fixed_debt
            .checked_add(isolated_debt)
            .and_then(|value| value.checked_add(hlp_funding_debt))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        utilization_bps(total_debt, self.side(asset).reserves.cash_reserve as u128)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct MarketHealth {
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub effective_base_debt_nad: u128,
    pub effective_quote_debt_nad: u128,
    pub base_debt_health_bps: u64,
    pub quote_debt_health_bps: u64,
}

#[cfg(test)]
mod limits_tests {
    include!("../../tests/transitions/lending_limits.rs");
}

#[cfg(test)]
mod market_interest_tests {
    include!("../../tests/transitions/lending_interest.rs");
}
