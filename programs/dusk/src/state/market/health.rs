use anchor_lang::prelude::*;

use super::{Market, MarketAsset, MarketHealth, Risk};
use crate::{
    constants::{BPS_DENOMINATOR, LIQUIDATION_INCENTIVE_BPS, LIQUIDATION_PENALTY_BPS},
    errors::ErrorCode,
    math::*,
    shared::math::ceil_div,
    state::BorrowPosition,
};

impl Market {
    pub fn market_health(&self) -> Result<MarketHealth> {
        self.market_health_from_risk(&self.risk)
    }

    pub fn market_health_from_risk(&self, risk: &Risk) -> Result<MarketHealth> {
        let effective_base_debt_nad = self.effective_base_debt_nad()?;
        let effective_quote_debt_nad = self.effective_quote_debt_nad()?;
        let base_debt_health_bps = if effective_base_debt_nad == 0 {
            u64::MAX
        } else {
            health_bps(
                self.quote_collateral_value_for_base_debt_nad_with_risk(
                    self.debt.utilized_quote_collateral_for_base_debt,
                    risk,
                )?,
                effective_base_debt_nad,
            )?
        };
        let quote_debt_health_bps = if effective_quote_debt_nad == 0 {
            u64::MAX
        } else {
            health_bps(
                self.base_collateral_value_for_quote_debt_nad_with_risk(
                    self.debt.utilized_base_collateral_for_quote_debt,
                    risk,
                )?,
                effective_quote_debt_nad,
            )?
        };
        Ok(MarketHealth {
            utilized_base_collateral_for_quote_debt: self.debt.utilized_base_collateral_for_quote_debt,
            utilized_quote_collateral_for_base_debt: self.debt.utilized_quote_collateral_for_base_debt,
            effective_base_debt_nad,
            effective_quote_debt_nad,
            base_debt_health_bps,
            quote_debt_health_bps,
        })
    }

    pub fn current_risk(&self) -> Result<Risk> {
        let current_slot = Clock::get().map(|clock| clock.slot).unwrap_or(self.last_update_slot);
        self.risk
            .refreshed(&self.base_side, &self.quote_side, &self.config, current_slot)
    }

    pub fn refresh_risk(&mut self) -> Result<()> {
        self.risk = self.current_risk()?;
        self.last_update_slot = self.risk.last_snapshot_slot;
        Ok(())
    }

    pub fn effective_base_debt_nad(&self) -> Result<u128> {
        self.effective_debt_nad(MarketAsset::Base)
    }

    pub fn effective_quote_debt_nad(&self) -> Result<u128> {
        self.effective_debt_nad(MarketAsset::Quote)
    }

    fn quote_collateral_value_for_base_debt_nad_with_risk(
        &self,
        quote_collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        self.collateral_value_nad(MarketAsset::Quote, quote_collateral_amount, risk)
    }

    fn base_collateral_value_for_quote_debt_nad_with_risk(
        &self,
        base_collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        self.collateral_value_nad(MarketAsset::Base, base_collateral_amount, risk)
    }

    pub(crate) fn collateral_amount_for_debt_value_with_penalty_bps(
        &self,
        debt_asset: MarketAsset,
        debt_amount: u64,
        penalty_bps: u16,
    ) -> Result<u64> {
        self.collateral_amount_for_debt_value_with_penalty(debt_asset, debt_amount, penalty_bps, &self.current_risk()?)
    }

    pub fn debt_capped_utilized_collateral(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        risk: &Risk,
    ) -> Result<u64> {
        let cap_bps = self.config.utilized_collateral_cap_bps as u128;
        let (fixed_debt, debt_decimals, total_collateral) = match debt_asset {
            MarketAsset::Base => (
                borrow_position.fixed_base_debt(&self.debt)?,
                self.base_side.asset_decimals,
                borrow_position.quote_collateral,
            ),
            MarketAsset::Quote => (
                borrow_position.fixed_quote_debt(&self.debt)?,
                self.quote_side.asset_decimals,
                borrow_position.base_collateral,
            ),
        };
        if fixed_debt == 0 || total_collateral == 0 {
            return Ok(0);
        }

        let debt_value_nad = normalize_to_nad(fixed_debt, debt_decimals)?;
        let utilized_value_cap_nad = debt_value_nad
            .checked_mul(cap_bps)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let capped_collateral =
            self.collateral_amount_for_debt_value_cap_with_risk(debt_asset, utilized_value_cap_nad, risk)?;
        Ok(total_collateral.min(capped_collateral))
    }

    pub fn position_health_bps_with_risk(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        risk: &Risk,
    ) -> Result<u64> {
        match debt_asset {
            MarketAsset::Base => health_bps(
                self.collateral_value_nad(
                    MarketAsset::Quote,
                    borrow_position.utilized_quote_collateral_for_base_debt,
                    risk,
                )?,
                normalize_to_nad(
                    borrow_position.fixed_base_debt(&self.debt)?,
                    self.base_side.asset_decimals,
                )?,
            ),
            MarketAsset::Quote => health_bps(
                self.collateral_value_nad(
                    MarketAsset::Base,
                    borrow_position.utilized_base_collateral_for_quote_debt,
                    risk,
                )?,
                normalize_to_nad(
                    borrow_position.fixed_quote_debt(&self.debt)?,
                    self.quote_side.asset_decimals,
                )?,
            ),
        }
    }

    pub fn assert_market_health(&self) -> Result<()> {
        let health = self.market_health()?;
        self.assert_market_health_snapshot(&health)
    }

    pub fn assert_market_health_snapshot(&self, health: &MarketHealth) -> Result<()> {
        if health.effective_base_debt_nad > 0 {
            require_gte!(
                health.base_debt_health_bps,
                self.config.market_health_min_bps as u64,
                ErrorCode::InsufficientMarketHealth
            );
        }
        if health.effective_quote_debt_nad > 0 {
            require_gte!(
                health.quote_debt_health_bps,
                self.config.market_health_min_bps as u64,
                ErrorCode::InsufficientMarketHealth
            );
        }
        Ok(())
    }

    fn effective_debt_nad(&self, debt_asset: MarketAsset) -> Result<u128> {
        let (fixed_debt, debt_side) = match debt_asset {
            MarketAsset::Base => (self.debt.fixed_base_debt()?, &self.base_side),
            MarketAsset::Quote => (self.debt.fixed_quote_debt()?, &self.quote_side),
        };
        normalize_to_nad(fixed_debt, debt_side.asset_decimals)
    }

    pub(crate) fn collateral_value_nad(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        let (collateral_side, debt_side, price_ema_nad, directional_price_ema_nad) = match collateral_asset {
            MarketAsset::Base => (
                &self.base_side,
                &self.quote_side,
                risk.base_price_ema_nad,
                risk.directional_base_price_ema_nad,
            ),
            MarketAsset::Quote => (
                &self.quote_side,
                &self.base_side,
                risk.quote_price_ema_nad,
                risk.directional_quote_price_ema_nad,
            ),
        };
        let (base_depth, quote_depth) = self.conservative_risk_reserve_depths(risk)?;
        let (collateral_reserve, debt_reserve) = match collateral_asset {
            MarketAsset::Base => (base_depth, quote_depth),
            MarketAsset::Quote => (quote_depth, base_depth),
        };

        collateral_value_from_pessimistic_reserves_nad(
            collateral_reserve,
            collateral_side.asset_decimals,
            debt_reserve,
            debt_side.asset_decimals,
            collateral_amount,
            price_ema_nad,
            directional_price_ema_nad,
        )
    }

    fn collateral_amount_for_debt_value_with_penalty(
        &self,
        debt_asset: MarketAsset,
        debt_amount: u64,
        penalty_bps: u16,
        risk: &Risk,
    ) -> Result<u64> {
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
        let (collateral_side, debt_side, price_ema_nad, directional_price_ema_nad) = match debt_asset {
            MarketAsset::Base => (
                &self.quote_side,
                &self.base_side,
                risk.quote_price_ema_nad,
                risk.directional_quote_price_ema_nad,
            ),
            MarketAsset::Quote => (
                &self.base_side,
                &self.quote_side,
                risk.base_price_ema_nad,
                risk.directional_base_price_ema_nad,
            ),
        };
        let (base_depth, quote_depth) = self.conservative_risk_reserve_depths(risk)?;
        let (collateral_reserve, debt_reserve) = match debt_asset {
            MarketAsset::Base => (quote_depth, base_depth),
            MarketAsset::Quote => (base_depth, quote_depth),
        };

        collateral_amount_for_debt_amount_ceil(
            collateral_reserve,
            collateral_side.asset_decimals,
            debt_reserve,
            debt_side.asset_decimals,
            debt_with_penalty,
            price_ema_nad,
            directional_price_ema_nad,
        )
    }

    fn collateral_amount_for_debt_value_cap_with_risk(
        &self,
        debt_asset: MarketAsset,
        debt_value_nad: u128,
        risk: &Risk,
    ) -> Result<u64> {
        let (collateral_side, debt_side, price_ema_nad, directional_price_ema_nad) = match debt_asset {
            MarketAsset::Base => (
                &self.quote_side,
                &self.base_side,
                risk.quote_price_ema_nad,
                risk.directional_quote_price_ema_nad,
            ),
            MarketAsset::Quote => (
                &self.base_side,
                &self.quote_side,
                risk.base_price_ema_nad,
                risk.directional_base_price_ema_nad,
            ),
        };
        let (base_depth, quote_depth) = self.conservative_risk_reserve_depths(risk)?;
        let (collateral_reserve, debt_reserve) = match debt_asset {
            MarketAsset::Base => (quote_depth, base_depth),
            MarketAsset::Quote => (base_depth, quote_depth),
        };

        collateral_amount_for_debt_value_floor(
            collateral_reserve,
            collateral_side.asset_decimals,
            debt_reserve,
            debt_side.asset_decimals,
            debt_value_nad,
            price_ema_nad,
            directional_price_ema_nad,
        )
    }

    pub(crate) fn conservative_risk_reserve_depths(&self, risk: &Risk) -> Result<(u64, u64)> {
        let base_spot = normalize_to_nad(
            self.base_side.reserves.live_reserve as u128,
            self.base_side.asset_decimals,
        )?;
        let quote_spot = normalize_to_nad(
            self.quote_side.reserves.live_reserve as u128,
            self.quote_side.asset_decimals,
        )?;
        let spot_k = base_spot.checked_mul(quote_spot).ok_or(ErrorCode::MarketMathOverflow)?;
        let conservative_k = if risk.k_ema == 0 {
            spot_k
        } else {
            spot_k.min(risk.k_ema)
        };
        let (base_depth_nad, quote_depth_nad) =
            construct_normalized_reserves_from_k_at_spot_ratio(base_spot, quote_spot, conservative_k)?;
        Ok((
            denormalize_from_nad_floor(base_depth_nad, self.base_side.asset_decimals)?
                .min(self.base_side.reserves.live_reserve),
            denormalize_from_nad_floor(quote_depth_nad, self.quote_side.asset_decimals)?
                .min(self.quote_side.reserves.live_reserve),
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
