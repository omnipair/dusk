use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    math::{mul_div_ceil_u128, mul_div_u128},
    state::{leverage_position::leverage_equity_potential, LeverageMarginTerms, LeveragePosition, Market, MarketAsset},
};

impl Market {
    pub fn leverage_collateral_exposure(&self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.debt.leverage_base_collateral,
            MarketAsset::Quote => self.debt.leverage_quote_collateral,
        }
    }

    pub fn leverage_reference_depth(&self, collateral_asset: MarketAsset) -> Result<u64> {
        let cash = self.side(collateral_asset).reserves.cash_reserve;
        let collateral_value = self.linear_liquidation_collateral_value_nad(collateral_asset, cash, &self.risk)?;
        require_gt!(collateral_value, 0, ErrorCode::InsufficientLiquidity);
        let debt_side = self.side(collateral_asset.opposite());
        let debt_cash = self.normalize_amount(u128::from(debt_side.reserves.cash_reserve), debt_side.asset_decimals)?;
        let cash_depth = mul_div_u128(u128::from(cash), debt_cash.min(collateral_value), collateral_value)?;
        let cache = self.amm.concentrated_curve_cache;
        let current_depth = cache
            .tail_liquidity
            .checked_add(cache.concentrated_liquidity)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gt!(current_depth, 0, ErrorCode::InsufficientLiquidity);
        let observed = self.risk.pessimistic_depth_nad().min(current_depth);
        let reference = mul_div_u128(cash_depth, observed, current_depth)?;
        require_gt!(reference, 0, ErrorCode::InsufficientLiquidity);
        u64::try_from(reference).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn leverage_admission_potential(&self, asset: MarketAsset) -> Result<u128> {
        leverage_equity_potential(
            self.leverage_collateral_exposure(asset),
            self.leverage_reference_depth(asset)?,
        )
    }

    pub(crate) fn retain_leverage_admission(
        &mut self,
        position: &mut LeveragePosition,
        added_collateral: u64,
        potential_before: u128,
        opening: bool,
    ) -> Result<()> {
        let asset = position.collateral_asset()?;
        let exposure = self
            .leverage_collateral_exposure(asset)
            .checked_add(added_collateral)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        match asset {
            MarketAsset::Base => self.debt.leverage_base_collateral = exposure,
            MarketAsset::Quote => self.debt.leverage_quote_collateral = exposure,
        }
        let reference = self.leverage_reference_depth(asset)?;
        if opening {
            position.margin_terms = LeverageMarginTerms::at_entry(reference)?;
        }
        let terms = &mut position.margin_terms;
        terms.reference_collateral = terms.reference_collateral.min(reference);
        let potential_after = leverage_equity_potential(exposure, reference)?;
        // A falling potential cannot become withdrawable credit.
        terms.admission_equity_collateral_nad = terms
            .admission_equity_collateral_nad
            .checked_add(potential_after.saturating_sub(potential_before))
            .ok_or(ErrorCode::MarketMathOverflow)?
            .max(terms.own_initial_equity_nad(position.collateral_amount)?);
        Ok(())
    }

    pub(crate) fn remove_leverage_exposure(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let exposure = self
            .leverage_collateral_exposure(asset)
            .checked_sub(amount)
            .ok_or(ErrorCode::BrokenInvariant)?;
        match asset {
            MarketAsset::Base => self.debt.leverage_base_collateral = exposure,
            MarketAsset::Quote => self.debt.leverage_quote_collateral = exposure,
        }
        Ok(())
    }

    pub(crate) fn reduce_leverage_collateral(&mut self, position: &mut LeveragePosition, amount: u64) -> Result<()> {
        let before = position.collateral_amount;
        let remaining = before.checked_sub(amount).ok_or(ErrorCode::InsufficientAmount)?;
        self.remove_leverage_exposure(position.collateral_asset()?, amount)?;
        position.margin_terms.admission_equity_collateral_nad = mul_div_ceil_u128(
            position.margin_terms.admission_equity_collateral_nad,
            u128::from(remaining),
            u128::from(before),
        )?;
        position.collateral_amount = remaining;
        Ok(())
    }

    pub fn leverage_reference_equity_bps(&self, position: &LeveragePosition, collateral_credit: u64) -> Result<u128> {
        let asset = position.collateral_asset()?;
        let value = self.linear_liquidation_collateral_value_nad(asset, collateral_credit, &self.risk)?;
        let debt = self.normalize_amount(
            u128::from(position.debt_amount(&self.debt)?),
            self.side(asset.opposite()).asset_decimals,
        )?;
        if value == 0 || debt >= value {
            return Ok(0);
        }
        mul_div_u128(value - debt, 10_000, value)
    }

    pub fn leverage_reference_protection_health(
        &self,
        position: &LeveragePosition,
        collateral_credit: u64,
    ) -> Result<u64> {
        let asset = position.collateral_asset()?;
        let debt = self.normalize_amount(
            u128::from(position.debt_amount(&self.debt)?),
            self.side(asset.opposite()).asset_decimals,
        )?;
        if debt == 0 {
            return Ok(u64::MAX);
        }
        let value = self.linear_liquidation_collateral_value_nad(asset, collateral_credit, &self.risk)?;
        let mm = position.margin_terms.maintenance_bps(position.collateral_amount)?;
        let health = mul_div_u128(value, u128::from(10_000 - mm - 1), debt)?;
        Ok(health.min(u128::from(u64::MAX)) as u64)
    }
}
