use anchor_lang::prelude::*;

use super::{Market, MarketAsset, MarketHealth, Risk};
use crate::{
    constants::{BPS_DENOMINATOR, LIQUIDATION_INCENTIVE_BPS, LIQUIDATION_PENALTY_BPS, LTV_BUFFER_BPS, NAD},
    errors::ErrorCode,
    math::*,
    shared::math::ceil_div,
    state::BorrowPosition,
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
        self.risk
            .refreshed(&self.base_side, &self.quote_side, &self.config, current_slot)
    }

    pub fn refresh_risk(&mut self) -> Result<()> {
        self.risk = self.current_risk()?;
        self.last_update_slot = self.risk.last_snapshot_slot;
        Ok(())
    }

    pub fn effective_base_debt_nad(&self) -> Result<u128> {
        Ok(self.market_health()?.effective_base_debt_nad)
    }

    pub fn effective_quote_debt_nad(&self) -> Result<u128> {
        Ok(self.market_health()?.effective_quote_debt_nad)
    }

    pub(crate) fn total_fixed_debt_nad(&self, debt_asset: MarketAsset) -> Result<u128> {
        let (fixed_debt, debt_decimals) = match debt_asset {
            MarketAsset::Base => (self.debt.fixed_base_debt()?, self.base_side.asset_decimals),
            MarketAsset::Quote => (self.debt.fixed_quote_debt()?, self.quote_side.asset_decimals),
        };
        normalize_to_nad(fixed_debt, debt_decimals)
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
        let (collateral_virtual_reserve_nad, debt_virtual_reserve_nad) =
            self.pessimistic_virtual_reserves_nad(collateral_asset, risk, true)?;
        let terms = pessimistic_max_debt_nad(
            collateral_amount_nad,
            collateral_virtual_reserve_nad,
            debt_virtual_reserve_nad,
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

    pub fn position_health_bps_with_risk(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        risk: &Risk,
    ) -> Result<u64> {
        let collateral_asset = debt_asset.opposite();
        let collateral_value_nad =
            self.collateral_value_nad(collateral_asset, borrow_position.collateral(collateral_asset), risk)?;
        let debt_nad = normalize_to_nad(
            match debt_asset {
                MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
                MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
            },
            self.side(debt_asset).asset_decimals,
        )?;
        health_bps(collateral_value_nad, debt_nad)
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

    pub(crate) fn buffered_debt_limit_for_liquidation_cf(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        liquidation_cf_bps: u16,
        risk: &Risk,
    ) -> Result<u64> {
        let collateral_value_nad = self.collateral_value_nad(collateral_asset, collateral_amount, risk)?;
        let max_cf_bps = max_cf_bps_from_liquidation_cf(liquidation_cf_bps);
        let max_debt_nad = collateral_value_nad
            .checked_mul(max_cf_bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        denormalize_from_nad_floor(max_debt_nad, self.side(collateral_asset.opposite()).asset_decimals)
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
        let (collateral_reserve_nad, debt_reserve_nad) =
            self.pessimistic_virtual_reserves_nad(collateral_asset, risk, true)?;
        calculate_normalized_amount_out(collateral_reserve_nad, debt_reserve_nad, collateral_amount_nad)
    }

    pub(crate) fn liquidation_collateral_value_nad(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        let collateral_amount_nad =
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let (collateral_reserve_nad, debt_reserve_nad) =
            self.pessimistic_virtual_reserves_nad(collateral_asset, risk, false)?;
        calculate_normalized_amount_out(collateral_reserve_nad, debt_reserve_nad, collateral_amount_nad)
    }

    pub(crate) fn collateral_amount_for_debt_value_with_penalty_bps(
        &self,
        debt_asset: MarketAsset,
        debt_amount: u64,
        penalty_bps: u16,
    ) -> Result<u64> {
        self.collateral_amount_for_debt_value_with_penalty(debt_asset, debt_amount, penalty_bps, &self.current_risk()?)
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
        let collateral_asset = debt_asset.opposite();
        let (collateral_reserve_nad, debt_reserve_nad) =
            self.pessimistic_virtual_reserves_nad(collateral_asset, risk, true)?;
        let debt_amount_nad = normalize_to_nad(debt_with_penalty, self.side(debt_asset).asset_decimals)?;
        let collateral_amount_nad =
            calculate_normalized_amount_in(collateral_reserve_nad, debt_reserve_nad, debt_amount_nad)?;
        denormalize_from_nad_ceil(collateral_amount_nad, self.side(collateral_asset).asset_decimals)
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
        let required_collateral_nad =
            calculate_normalized_amount_in(collateral_reserve_nad, debt_reserve_nad, projected_total_debt_nad)?;
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
        let (base_depth, quote_depth) = self.conservative_risk_reserve_depths(risk)?;
        let (collateral_depth, debt_depth) = match collateral_asset {
            MarketAsset::Base => (base_depth, quote_depth),
            MarketAsset::Quote => (quote_depth, base_depth),
        };
        let collateral_depth_nad =
            normalize_to_nad(collateral_depth as u128, self.side(collateral_asset).asset_decimals)?;
        let debt_depth_nad = normalize_to_nad(
            debt_depth as u128,
            self.side(collateral_asset.opposite()).asset_decimals,
        )?;
        let symmetric_price = self.pessimistic_collateral_price_nad(collateral_asset, risk, false);
        let directional_price = if include_directional_ema {
            self.pessimistic_collateral_price_nad(collateral_asset, risk, true)
        } else {
            symmetric_price
        };
        construct_normalized_virtual_reserves_at_pessimistic_price(
            collateral_depth_nad,
            debt_depth_nad,
            symmetric_price,
            directional_price,
        )
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
