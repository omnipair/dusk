use anchor_lang::prelude::*;

use crate::{
    constants::{
        BPS_DENOMINATOR, LIQUIDATION_CLOSE_FACTOR_BPS, LIQUIDATION_INCENTIVE_BPS, LIQUIDATION_INSURANCE_FUNDING_BPS,
        LIQUIDATION_MAX_INCENTIVE_BPS, LIQUIDATION_PENALTY_BPS, LTV_BUFFER_BPS, MAX_COLLATERAL_FACTOR_BPS, NAD,
    },
    errors::ErrorCode,
    math::*,
    state::{BorrowPosition, Debt, Market, MarketAsset, MarketHealth, Risk},
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
            .current_explicit_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let q_nad = self
            .amm
            .explicit_curve_cache
            .tail_liquidity
            .checked_add(self.amm.explicit_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let quote_price_nad = u64::try_from(
            (NAD as u128)
                .checked_mul(NAD as u128)
                .and_then(|value| value.checked_div(price_nad as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        self.risk
            .refreshed(price_nad, quote_price_nad, q_nad, &self.config, current_slot)
    }

    /// O(1) risk observation for the explicit tail+band curve. The quote has
    /// already evaluated the exact final marginal price; total explicit
    /// liquidity replaces the legacy balanced-equivalent root.
    pub(crate) fn observe_risk_from_explicit_curve(
        &mut self,
        current_base_price_nad: u64,
        current_q_nad: u128,
        current_slot: u64,
    ) -> Result<()> {
        require!(
            current_base_price_nad > 0 && current_q_nad > 0,
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
            current_q_nad,
            &self.config,
            current_slot,
        )?;
        self.last_marginal_observation_nad = current_base_price_nad;
        self.last_update_slot = current_slot;
        Ok(())
    }

    /// Observes exact current price and Q without persisting pessimistic
    /// reserve shapes. Risk consumers reconstruct the applied shape from this
    /// scalar snapshot and the current curve parameters.
    pub(crate) fn observe_current_risk(&mut self, current_slot: u64) -> Result<()> {
        let price_nad = self
            .current_explicit_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let q_nad = self
            .amm
            .explicit_curve_cache
            .tail_liquidity
            .checked_add(self.amm.explicit_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        self.observe_risk_from_explicit_curve(price_nad, q_nad, current_slot)?;
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
            self.risk.cached_q_nad = 0;
            self.risk.last_snapshot_slot = current_slot;
            self.amm.explicit_curve_cache = Default::default();
            self.last_marginal_observation_nad = 0;
            self.risk_revision = self.curve_revision;
            self.last_update_slot = current_slot;
            return Ok(());
        }

        let price_nad = self
            .current_explicit_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let q_nad = self
            .amm
            .explicit_curve_cache
            .tail_liquidity
            .checked_add(self.amm.explicit_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        self.observe_risk_from_explicit_curve(price_nad, q_nad, current_slot)?;
        self.risk_revision = self.curve_revision;
        Ok(())
    }

    fn pessimistic_explicit_curve(
        &self,
        collateral_asset: MarketAsset,
        risk: &Risk,
        include_directional_ema: bool,
    ) -> Result<Option<(ExplicitCurveGeometry, ExplicitCurvePoint, ExplicitCurveDirection)>> {
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
        let geometry = self
            .current_explicit_curve_geometry()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        // Reconstruct the same explicit shape at the pessimistic price, but
        // cap its total depth by the conservative Q observation. Scaling the
        // tail scales the concentrated tranche by the configured share, so
        // this preserves concentration while preventing stale/full live
        // depth from leaking into lending and liquidation valuations.
        let current_total_liquidity = self
            .amm
            .explicit_curve_cache
            .tail_liquidity
            .checked_add(self.amm.explicit_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(current_total_liquidity > 0, ErrorCode::InsufficientLiquidity);
        let conservative_q_nad = risk.conservative_q_nad();
        let target_total_liquidity = if conservative_q_nad == 0 {
            current_total_liquidity
        } else {
            conservative_q_nad.min(current_total_liquidity)
        };
        let target_tail_liquidity = mul_div_u128(
            self.amm.explicit_curve_cache.tail_liquidity,
            target_total_liquidity,
            current_total_liquidity,
        )?
        .max(1);
        let point = geometry.point_at_price_nad(base_price_nad, target_tail_liquidity)?;
        let direction = match collateral_asset {
            MarketAsset::Base => ExplicitCurveDirection::BaseToQuote,
            MarketAsset::Quote => ExplicitCurveDirection::QuoteToBase,
        };
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
            .pessimistic_explicit_curve(collateral_asset, risk, true)?
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
        if collateral_amount == 0 {
            return Ok(0);
        }
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let (geometry, point, direction) = self
            .pessimistic_explicit_curve(collateral_asset, risk, true)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        geometry
            .quote_exact_in(point, collateral_amount_nad, direction)
            .map(|quote| quote.amount_out)
    }

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
            .pessimistic_explicit_curve(collateral_asset, risk, false)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        geometry
            .quote_exact_in(point, collateral_amount_nad, direction)
            .map(|quote| quote.amount_out)
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
        let geometry = self
            .current_explicit_curve_geometry()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let (point, direction) = match collateral_asset {
            MarketAsset::Base => (
                ExplicitCurvePoint {
                    base_reserve: collateral_reserve_nad,
                    quote_reserve: debt_reserve_nad,
                },
                ExplicitCurveDirection::BaseToQuote,
            ),
            MarketAsset::Quote => (
                ExplicitCurvePoint {
                    base_reserve: debt_reserve_nad,
                    quote_reserve: collateral_reserve_nad,
                },
                ExplicitCurveDirection::QuoteToBase,
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
            .pessimistic_explicit_curve(collateral_asset, risk, include_directional_ema)?
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

    pub(crate) fn conservative_risk_reserve_depths(&self, risk: &Risk) -> Result<(u64, u64)> {
        let (base_nad, quote_nad) = self.pessimistic_virtual_reserves_nad(MarketAsset::Base, risk, true)?;
        Ok((
            denormalize_from_nad_floor(base_nad, self.base_side.asset_decimals)?
                .min(self.curve_reserve(MarketAsset::Base)?),
            denormalize_from_nad_floor(quote_nad, self.quote_side.asset_decimals)?
                .min(self.curve_reserve(MarketAsset::Quote)?),
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

#[cfg(test)]
fn liquidation_health_floor_bps(liquidation_cf_bps: u16) -> u64 {
    if liquidation_cf_bps == 0 {
        return u64::MAX;
    }
    ceil_div((BPS_DENOMINATOR as u128).pow(2), liquidation_cf_bps as u128)
        .unwrap_or(u128::from(u64::MAX))
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
fn liquidation_max_incentive_bps(health_bps: u64, min_health_bps: u64) -> u16 {
    let shortfall = min_health_bps.saturating_sub(health_bps);
    let max_for_config = min_health_bps
        .saturating_sub(BPS_DENOMINATOR as u64 + 1)
        .min(LIQUIDATION_MAX_INCENTIVE_BPS as u64);
    shortfall.max(LIQUIDATION_INCENTIVE_BPS as u64).min(max_for_config) as u16
}

pub struct Liquidation {
    pub debt_asset: MarketAsset,
    pub repay_credit: u64,
    pub insurance_spent: u64,
    pub insurance_credit: u64,
    pub max_socialized_loss: u64,
    pub terms: LiquidationTerms,
    pub pricing: LiquidationPricing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidationPricing {
    PessimisticReserves,
    ReferencePrice { debt_per_collateral_price_nad: u64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiquidationTerms {
    pub liquidation_incentive_bps: u16,
    pub insurance_funding_bps: u16,
    pub total_penalty_bps: u16,
    pub max_repay_amount: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiquidationReceipt {
    pub repaid_amount: u64,
    pub interest_paid: u64,
    pub collateral_seized: u64,
    pub collateral_to_liquidator: u64,
    pub insurance_funded: u64,
    pub insurance_drawn: u64,
    pub socialized_loss: u64,
    pub remaining_debt: u128,
    pub remaining_global_health_contribution: u64,
    pub remaining_liquidation_cf_bps: u16,
    pub liquidation_incentive_bps: u16,
    pub insurance_funding_bps: u16,
    pub max_repay_amount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiquidationDebtClearance {
    shares_to_burn: u128,
    aggregate_debt_reduction: u64,
}

impl Liquidation {
    #[cfg(test)]
    pub fn new(
        debt_asset: MarketAsset,
        repay_credit: u64,
        insurance_spent: u64,
        insurance_credit: u64,
        max_socialized_loss: u64,
        terms: LiquidationTerms,
    ) -> Self {
        Self {
            debt_asset,
            repay_credit,
            insurance_spent,
            insurance_credit,
            max_socialized_loss,
            terms,
            pricing: LiquidationPricing::PessimisticReserves,
        }
    }

    pub fn new_with_pricing(
        debt_asset: MarketAsset,
        repay_credit: u64,
        insurance_spent: u64,
        insurance_credit: u64,
        max_socialized_loss: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
    ) -> Self {
        Self {
            debt_asset,
            repay_credit,
            insurance_spent,
            insurance_credit,
            max_socialized_loss,
            terms,
            pricing,
        }
    }

    pub fn apply(self, market: &mut Market, borrow_position: &mut BorrowPosition) -> Result<LiquidationReceipt> {
        let debt_before = position_debt(market, borrow_position, self.debt_asset)?;
        require_gte!(debt_before, self.repay_credit as u128, ErrorCode::InsufficientDebt);
        require_gte!(
            self.terms.max_repay_amount,
            self.repay_credit,
            ErrorCode::LiquidationRepayTooLarge
        );
        let collateral_before = position_collateral(borrow_position, self.debt_asset);
        let collateral_seized = collateral_to_seize(
            market,
            self.debt_asset,
            self.repay_credit,
            collateral_before,
            self.terms.total_penalty_bps,
            self.pricing,
        )?;
        let collateral_to_liquidator = collateral_amount_for_debt_value_with_pricing(
            market,
            self.debt_asset,
            self.repay_credit,
            self.terms.liquidation_incentive_bps,
            self.pricing,
        )?
        .min(collateral_seized);
        let insurance_funded = collateral_seized
            .checked_sub(collateral_to_liquidator)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let collateral_exhausted = collateral_seized == collateral_before;
        let repay_plus_insurance = (self.repay_credit as u128)
            .checked_add(self.insurance_credit as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(debt_before, repay_plus_insurance, ErrorCode::InsufficientDebt);
        let cap_remaining = self
            .terms
            .max_repay_amount
            .checked_sub(self.repay_credit)
            .ok_or(ErrorCode::LiquidationRepayTooLarge)?;
        require_gte!(
            cap_remaining,
            self.insurance_credit,
            ErrorCode::LiquidationRepayTooLarge
        );

        let debt_clearance = {
            let (shares_before, position_debt_before, borrow_index_nad) = match self.debt_asset {
                MarketAsset::Base => (
                    borrow_position.fixed_base_shares,
                    borrow_position.fixed_base_debt(&market.debt)?,
                    market.debt.base_borrow_index_nad,
                ),
                MarketAsset::Quote => (
                    borrow_position.fixed_quote_shares,
                    borrow_position.fixed_quote_debt(&market.debt)?,
                    market.debt.quote_borrow_index_nad,
                ),
            };
            require!(
                shares_before > 0 && position_debt_before > 0,
                ErrorCode::InsufficientDebt
            );
            let repayment = if collateral_exhausted {
                Debt::repayment_for_max(
                    shares_before,
                    match self.debt_asset {
                        MarketAsset::Base => market.debt.fixed_base_shares,
                        MarketAsset::Quote => market.debt.fixed_quote_shares,
                    },
                    borrow_index_nad,
                    u64::MAX,
                )?
            } else {
                let cash_repaid = u64::try_from(repay_plus_insurance).map_err(|_| ErrorCode::MarketMathOverflow)?;
                let quote = Debt::repayment_for_max(
                    shares_before,
                    match self.debt_asset {
                        MarketAsset::Base => market.debt.fixed_base_shares,
                        MarketAsset::Quote => market.debt.fixed_quote_shares,
                    },
                    borrow_index_nad,
                    cash_repaid,
                )?;
                require_eq!(quote.cash_repaid, cash_repaid, ErrorCode::DebtShareDivisionOverflow);
                quote
            };
            LiquidationDebtClearance {
                shares_to_burn: repayment.shares_to_burn,
                aggregate_debt_reduction: repayment.cash_repaid,
            }
        };
        let cash_repaid = u64::try_from(repay_plus_insurance).map_err(|_| ErrorCode::MarketMathOverflow)?;
        require_gte!(
            debt_clearance.aggregate_debt_reduction,
            cash_repaid,
            ErrorCode::MarketMathOverflow
        );
        let socialized_loss = if collateral_exhausted {
            debt_clearance
                .aggregate_debt_reduction
                .checked_sub(cash_repaid)
                .ok_or(ErrorCode::MarketMathOverflow)?
        } else {
            0
        };
        require_gte!(
            self.max_socialized_loss,
            socialized_loss,
            ErrorCode::LiquidationSocializationExceeded
        );
        // Track the principal/interest split for cash-backed repayment without
        // treating socialized loss or share-rounding writeoff as received
        // interest.
        let interest_paid = market.debt.realize_margin_liquidation(
            self.debt_asset,
            cash_repaid,
            debt_clearance.aggregate_debt_reduction,
        )?;
        let principal_credit = cash_repaid
            .checked_sub(interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        match self.debt_asset {
            MarketAsset::Base => {
                borrow_position.quote_collateral = borrow_position
                    .quote_collateral
                    .checked_sub(collateral_seized)
                    .ok_or(ErrorCode::InsufficientBalance)?;
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                market.debt.fixed_base_shares = market
                    .debt
                    .fixed_base_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            MarketAsset::Quote => {
                borrow_position.base_collateral = borrow_position
                    .base_collateral
                    .checked_sub(collateral_seized)
                    .ok_or(ErrorCode::InsufficientBalance)?;
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                market.debt.fixed_quote_shares = market
                    .debt
                    .fixed_quote_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }

        {
            let debt_side = market.side_mut(self.debt_asset);
            let live_debit = debt_clearance
                .aggregate_debt_reduction
                .checked_sub(principal_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_sub(live_debit)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            debt_side.reserves.cash_reserve = debt_side
                .reserves
                .cash_reserve
                .checked_add(principal_credit)
                .ok_or(ErrorCode::ReserveOverflow)?;
        }
        match self.debt_asset {
            MarketAsset::Base => {
                market.insurance.base_available = market
                    .insurance
                    .base_available
                    .checked_sub(self.insurance_spent)
                    .ok_or(ErrorCode::InsufficientInsurance)?;
                market.insurance.quote_available = market
                    .insurance
                    .quote_available
                    .checked_add(insurance_funded)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            MarketAsset::Quote => {
                market.insurance.quote_available = market
                    .insurance
                    .quote_available
                    .checked_sub(self.insurance_spent)
                    .ok_or(ErrorCode::InsufficientInsurance)?;
                market.insurance.base_available = market
                    .insurance
                    .base_available
                    .checked_add(insurance_funded)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }

        market.refresh_risk()?;
        let remaining_debt = position_debt(market, borrow_position, self.debt_asset)?;
        let remaining_collateral = position_collateral(borrow_position, self.debt_asset);
        let target_contribution = market.debt_capped_global_health_contribution(
            self.debt_asset,
            remaining_debt,
            remaining_collateral,
            &market.risk,
        )?;
        if remaining_debt == 0 {
            borrow_position.set_liquidation_cf_bps(self.debt_asset, 0);
            borrow_position.clear_referral_binding(self.debt_asset);
        } else {
            let total_debt_nad = market.total_fixed_debt_nad(self.debt_asset)?;
            let external_debt_nad = market.external_fixed_debt_nad(borrow_position, self.debt_asset)?;
            let projected_aggregate = market.projected_aggregate_global_health_contribution(
                borrow_position,
                self.debt_asset,
                target_contribution,
            )?;
            let terms = market.dynamic_borrow_terms(
                self.debt_asset,
                remaining_collateral,
                external_debt_nad,
                total_debt_nad,
                projected_aggregate,
                &market.risk,
            )?;
            borrow_position.set_liquidation_cf_bps(self.debt_asset, terms.liquidation_cf_bps);
        }
        market.reconcile_global_health_contribution(borrow_position, self.debt_asset, target_contribution)?;
        market.reconcile_liquidation_auction(borrow_position)?;

        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;

        Ok(LiquidationReceipt {
            repaid_amount: self.repay_credit,
            interest_paid,
            collateral_seized,
            collateral_to_liquidator,
            insurance_funded,
            insurance_drawn: self.insurance_credit,
            socialized_loss,
            remaining_debt: position_debt(market, borrow_position, self.debt_asset)?,
            remaining_global_health_contribution: borrow_position.global_health_contribution(self.debt_asset),
            remaining_liquidation_cf_bps: borrow_position.liquidation_cf_bps(self.debt_asset),
            liquidation_incentive_bps: self.terms.liquidation_incentive_bps,
            insurance_funding_bps: self.terms.insurance_funding_bps,
            max_repay_amount: self.terms.max_repay_amount,
        })
    }
}

fn position_debt(market: &Market, borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> Result<u128> {
    match debt_asset {
        MarketAsset::Base => borrow_position.fixed_base_debt(&market.debt),
        MarketAsset::Quote => borrow_position.fixed_quote_debt(&market.debt),
    }
}

fn position_collateral(borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> u64 {
    match debt_asset {
        MarketAsset::Base => borrow_position.quote_collateral,
        MarketAsset::Quote => borrow_position.base_collateral,
    }
}

fn collateral_to_seize(
    market: &Market,
    debt_asset: MarketAsset,
    repay_credit: u64,
    collateral_before: u64,
    total_penalty_bps: u16,
    pricing: LiquidationPricing,
) -> Result<u64> {
    let seizure =
        collateral_amount_for_debt_value_with_pricing(market, debt_asset, repay_credit, total_penalty_bps, pricing)?;
    Ok(seizure.min(collateral_before))
}

pub(crate) fn liquidation_health_bps_with_pricing(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    pricing: LiquidationPricing,
) -> Result<u64> {
    let collateral_value_nad = position_collateral_value_with_pricing(market, borrow_position, debt_asset, pricing)?;
    let (debt_before, debt_decimals) = match debt_asset {
        MarketAsset::Base => (
            borrow_position.fixed_base_debt(&market.debt)?,
            market.base_side.asset_decimals,
        ),
        MarketAsset::Quote => (
            borrow_position.fixed_quote_debt(&market.debt)?,
            market.quote_side.asset_decimals,
        ),
    };
    health_bps(collateral_value_nad, normalize_to_nad(debt_before, debt_decimals)?)
}

#[cfg(test)]
fn max_repay_to_restore_health_with_pricing(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    total_penalty_bps: u16,
    pricing: LiquidationPricing,
) -> Result<u64> {
    let debt_before = position_debt(market, borrow_position, debt_asset)?;
    let debt_decimals = market.side(debt_asset).asset_decimals;
    let debt_value_nad = normalize_to_nad(debt_before, debt_decimals)?;
    let collateral_value_nad = position_collateral_value_with_pricing(market, borrow_position, debt_asset, pricing)?;
    let target_bps = liquidation_health_floor_bps(borrow_position.liquidation_cf_bps(debt_asset)) as u128;
    let penalty_multiplier_bps = (BPS_DENOMINATOR as u128)
        .checked_add(total_penalty_bps as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(target_bps > penalty_multiplier_bps, ErrorCode::InvalidMarketConfig);
    let target_debt_value = debt_value_nad
        .checked_mul(target_bps)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let collateral_value = collateral_value_nad
        .checked_mul(BPS_DENOMINATOR as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if target_debt_value <= collateral_value {
        return Ok(0);
    }
    let repay_value_nad = ceil_div(
        target_debt_value
            .checked_sub(collateral_value)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        target_bps
            .checked_sub(penalty_multiplier_bps)
            .ok_or(ErrorCode::MarketMathOverflow)?,
    )
    .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(denormalize_from_nad_ceil(repay_value_nad, debt_decimals)?.min(u64::try_from(debt_before).unwrap_or(u64::MAX)))
}

fn position_collateral_value_with_pricing(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    pricing: LiquidationPricing,
) -> Result<u128> {
    match pricing {
        LiquidationPricing::PessimisticReserves => {
            let risk = market.current_risk()?;
            match debt_asset {
                MarketAsset::Base => {
                    market.liquidation_collateral_value_nad(MarketAsset::Quote, borrow_position.quote_collateral, &risk)
                }
                MarketAsset::Quote => {
                    market.liquidation_collateral_value_nad(MarketAsset::Base, borrow_position.base_collateral, &risk)
                }
            }
        }
        LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad,
        } => {
            let collateral_asset = debt_asset.opposite();
            let collateral_amount = position_collateral(borrow_position, debt_asset);
            require!(debt_per_collateral_price_nad > 0, ErrorCode::InvalidSettlementPrice);
            let collateral_amount_nad =
                normalize_to_nad(collateral_amount as u128, market.side(collateral_asset).asset_decimals)?;
            collateral_amount_nad
                .checked_mul(debt_per_collateral_price_nad as u128)
                .and_then(|value| value.checked_div(NAD as u128))
                .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
        }
    }
}

fn collateral_amount_for_debt_value_with_pricing(
    market: &Market,
    debt_asset: MarketAsset,
    debt_amount: u64,
    penalty_bps: u16,
    pricing: LiquidationPricing,
) -> Result<u64> {
    match pricing {
        LiquidationPricing::PessimisticReserves => {
            let risk = market.current_risk()?;
            require_gte!(
                LIQUIDATION_PENALTY_BPS,
                LIQUIDATION_INCENTIVE_BPS,
                ErrorCode::InvalidMarketConfig
            );
            let debt_with_penalty = ceil_div(
                (debt_amount as u128)
                    .checked_mul((BPS_DENOMINATOR + penalty_bps) as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let collateral_asset = debt_asset.opposite();
            let debt_amount_nad = normalize_to_nad(debt_with_penalty, market.side(debt_asset).asset_decimals)?;
            let (geometry, point, direction) = market
                .pessimistic_explicit_curve(collateral_asset, &risk, true)?
                .ok_or(ErrorCode::BrokenInvariant)?;
            let collateral_amount_nad = geometry.quote_exact_out(point, debt_amount_nad, direction)?.amount_in;
            denormalize_from_nad_ceil(collateral_amount_nad, market.side(collateral_asset).asset_decimals)
        }
        LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad,
        } => {
            require!(debt_per_collateral_price_nad > 0, ErrorCode::InvalidSettlementPrice);
            let debt_decimals = market.side(debt_asset).asset_decimals;
            let collateral_decimals = market.side(debt_asset.opposite()).asset_decimals;
            let debt_with_penalty = ceil_div(
                (debt_amount as u128)
                    .checked_mul((BPS_DENOMINATOR + penalty_bps) as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let debt_value_nad = normalize_to_nad(debt_with_penalty, debt_decimals)?;
            let collateral_amount_nad = ceil_div(
                debt_value_nad
                    .checked_mul(NAD as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                debt_per_collateral_price_nad as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            denormalize_from_nad_ceil(collateral_amount_nad, collateral_decimals)
        }
    }
}

impl Market {
    pub fn liquidation_reference_price_nad(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
    ) -> Result<u64> {
        let risk = self.current_risk()?;
        let collateral_asset = debt_asset.opposite();
        let collateral_amount = position_collateral(borrow_position, debt_asset);
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        require!(collateral_amount_nad > 0, ErrorCode::InvalidSettlementPrice);
        let collateral_value_nad = self.liquidation_collateral_value_nad(collateral_asset, collateral_amount, &risk)?;
        let price_nad = collateral_value_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(collateral_amount_nad))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let price_nad = u64::try_from(price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(price_nad > 0, ErrorCode::InvalidSettlementPrice);
        Ok(price_nad)
    }

    pub fn liquidation_health_bps_with_pricing(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        pricing: LiquidationPricing,
    ) -> Result<u64> {
        liquidation_health_bps_with_pricing(self, borrow_position, debt_asset, pricing)
    }

    pub fn liquidation_terms_with_pricing(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        pricing: LiquidationPricing,
    ) -> Result<LiquidationTerms> {
        let health_before = liquidation_health_bps_with_pricing(self, borrow_position, debt_asset, pricing)?;
        let liquidation_cf_bps = borrow_position.liquidation_cf_bps(debt_asset);
        let liquidation_health_floor_bps = if liquidation_cf_bps == 0 {
            u64::MAX
        } else {
            ceil_div((BPS_DENOMINATOR as u128).pow(2), liquidation_cf_bps as u128)
                .unwrap_or(u128::from(u64::MAX))
                .min(u128::from(u64::MAX)) as u64
        };
        let shortfall = liquidation_health_floor_bps.saturating_sub(health_before);
        let max_for_config = liquidation_health_floor_bps
            .saturating_sub(BPS_DENOMINATOR as u64 + 1)
            .min(LIQUIDATION_MAX_INCENTIVE_BPS as u64);
        let liquidation_incentive_bps = shortfall.max(LIQUIDATION_INCENTIVE_BPS as u64).min(max_for_config) as u16;
        let max_total_penalty = liquidation_health_floor_bps.saturating_sub(BPS_DENOMINATOR as u64 + 1);
        let remaining_penalty_room = max_total_penalty.saturating_sub(liquidation_incentive_bps as u64);
        let insurance_funding_bps =
            LIQUIDATION_INSURANCE_FUNDING_BPS.min(u16::try_from(remaining_penalty_room).unwrap_or(u16::MAX));
        let total_penalty_bps = liquidation_incentive_bps
            .checked_add(insurance_funding_bps)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let max_repay_amount = {
            let debt_before = position_debt(self, borrow_position, debt_asset)?;
            if debt_before == 0 {
                0
            } else {
                let debt_decimals = self.side(debt_asset).asset_decimals;
                let debt_value_nad = normalize_to_nad(debt_before, debt_decimals)?;
                let collateral_value_nad =
                    position_collateral_value_with_pricing(self, borrow_position, debt_asset, pricing)?;
                let target_bps = liquidation_health_floor_bps as u128;
                let penalty_multiplier_bps = (BPS_DENOMINATOR as u128)
                    .checked_add(total_penalty_bps as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                require!(target_bps > penalty_multiplier_bps, ErrorCode::InvalidMarketConfig);
                let target_debt_value = debt_value_nad
                    .checked_mul(target_bps)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let collateral_value = collateral_value_nad
                    .checked_mul(BPS_DENOMINATOR as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let restore_cap = if target_debt_value <= collateral_value {
                    0
                } else {
                    let repay_value_nad = ceil_div(
                        target_debt_value
                            .checked_sub(collateral_value)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                        target_bps
                            .checked_sub(penalty_multiplier_bps)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                    )
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                    denormalize_from_nad_ceil(repay_value_nad, debt_decimals)?
                        .min(u64::try_from(debt_before).unwrap_or(u64::MAX))
                };
                if restore_cap == 0 {
                    0
                } else {
                    let close_factor_cap = ceil_div(
                        debt_before
                            .checked_mul(LIQUIDATION_CLOSE_FACTOR_BPS as u128)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                        BPS_DENOMINATOR as u128,
                    )
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                    let debt_before_u64 = u64::try_from(debt_before).unwrap_or(u64::MAX);
                    let mut max_repay = restore_cap.min(u64::try_from(close_factor_cap).unwrap_or(u64::MAX));
                    let leaves_dust = if max_repay as u128 >= debt_before {
                        false
                    } else if debt_before.saturating_sub(max_repay as u128) <= 1 {
                        true
                    } else {
                        self.fixed_repayment_for_max(borrow_position, debt_asset, max_repay)
                            .map(|repayment| {
                                repayment.shares_to_burn
                                    == match debt_asset {
                                        MarketAsset::Base => borrow_position.fixed_base_shares,
                                        MarketAsset::Quote => borrow_position.fixed_quote_shares,
                                    }
                            })
                            .unwrap_or(false)
                    };
                    if max_repay >= debt_before_u64 || leaves_dust {
                        max_repay = debt_before_u64;
                    }
                    max_repay
                }
            }
        };
        Ok(LiquidationTerms {
            liquidation_incentive_bps,
            insurance_funding_bps,
            total_penalty_bps,
            max_repay_amount,
        })
    }

    pub fn insurance_request_for_liquidation_with_terms_and_pricing(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        repay_credit: u64,
        max_insurance_draw: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
    ) -> Result<u64> {
        let debt_before = position_debt(self, borrow_position, debt_asset)?;
        require_gte!(debt_before, repay_credit as u128, ErrorCode::InsufficientDebt);
        require_gte!(
            terms.max_repay_amount,
            repay_credit,
            ErrorCode::LiquidationRepayTooLarge
        );
        let collateral_before = position_collateral(borrow_position, debt_asset);
        let collateral_seized = collateral_to_seize(
            self,
            debt_asset,
            repay_credit,
            collateral_before,
            terms.total_penalty_bps,
            pricing,
        )?;
        let remaining_debt = debt_before
            .checked_sub(repay_credit as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if collateral_seized < collateral_before || remaining_debt == 0 {
            return Ok(0);
        }
        let available = match debt_asset {
            MarketAsset::Base => self.insurance.base_available,
            MarketAsset::Quote => self.insurance.quote_available,
        };
        let remaining_debt_cap = u64::try_from(remaining_debt).unwrap_or(u64::MAX);
        let remaining_partial_cap = terms
            .max_repay_amount
            .checked_sub(repay_credit)
            .ok_or(ErrorCode::LiquidationRepayTooLarge)?;
        Ok(remaining_debt_cap
            .min(available)
            .min(max_insurance_draw)
            .min(remaining_partial_cap))
    }

    pub fn settle_liquidation(
        &mut self,
        borrow_position: &mut BorrowPosition,
        debt_asset: MarketAsset,
        repay_credit: u64,
        insurance_spent: u64,
        insurance_credit: u64,
        max_socialized_loss: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
    ) -> Result<LiquidationReceipt> {
        Liquidation::new_with_pricing(
            debt_asset,
            repay_credit,
            insurance_spent,
            insurance_credit,
            max_socialized_loss,
            terms,
            pricing,
        )
        .apply(self, borrow_position)
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/market/liquidation.rs");
}
