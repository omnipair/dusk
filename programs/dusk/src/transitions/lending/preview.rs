use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LendingSidePreview {
    pub conservative_depth_nad: u128,
    pub borrow_index_nad: u128,
    pub rate_at_target_nad: u128,
    pub borrow_apr_nad: u128,
    pub utilization_bps: u64,
    pub fixed_debt: u128,
    pub isolated_debt: u128,
    pub hlp_funding_debt: u128,
    pub total_debt: u128,
    pub daily_borrow_limit: u64,
    pub daily_borrow_remaining: u64,
    pub spot_price_nad: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BorrowCapacityQuote {
    pub collateral_value_nad: u128,
    pub max_debt_by_health: u64,
    pub max_debt_by_cash: u64,
    pub max_debt_by_daily_limit: u64,
    pub max_debt: u64,
    pub projected_debt_amount: u64,
    pub projected_health_bps: u64,
    pub projected_terms: DynamicBorrowTerms,
    pub projected_global_health_contribution: u64,
    pub liquidation_debt_per_collateral_price_nad: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PositionDebtSideQuote {
    pub debt_asset: MarketAsset,
    pub collateral_asset: MarketAsset,
    pub fixed_debt: u128,
    pub collateral_amount: u64,
    pub global_health_contribution: u64,
    pub collateral_value_nad: u128,
    pub health_bps: u64,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
    pub liquidation_reference_price_nad: u64,
    pub liquidation_health_bps: u64,
    pub is_liquidatable: bool,
    pub liquidation_incentive_bps: u16,
    pub insurance_funding_bps: u16,
    pub total_penalty_bps: u16,
    pub max_repay_amount: u64,
}

impl Market {
    pub(crate) fn lending_side_preview(&self, asset: MarketAsset, slot: u64) -> Result<LendingSidePreview> {
        let side = self.side(asset);
        let (base_depth, quote_depth) = self.pessimistic_borrow_reserve_depths(&self.risk)?;
        let conservative_depth_nad = match asset {
            MarketAsset::Base => normalize_to_nad(base_depth as u128, side.asset_decimals)?,
            MarketAsset::Quote => normalize_to_nad(quote_depth as u128, side.asset_decimals)?,
        };
        let borrow_index_nad = self.debt.borrow_index(asset);
        let rate_at_target_nad = match asset {
            MarketAsset::Base => self.debt.base_rate_at_target_nad,
            MarketAsset::Quote => self.debt.quote_rate_at_target_nad,
        };
        let fixed_debt = match asset {
            MarketAsset::Base => self.debt.fixed_base_debt()?,
            MarketAsset::Quote => self.debt.fixed_quote_debt()?,
        };
        let isolated_debt = self.debt.isolated_debt(asset)?;
        let (hlp_debt_shares, hlp_borrow_index_nad) = match asset {
            MarketAsset::Base => (self.quote_hlp_vault.debt_shares, self.debt.base_borrow_index_nad),
            MarketAsset::Quote => (self.base_hlp_vault.debt_shares, self.debt.quote_borrow_index_nad),
        };
        let hlp_funding_debt = Debt::shares_to_debt(hlp_debt_shares, hlp_borrow_index_nad)?;
        let total_debt = fixed_debt
            .checked_add(isolated_debt)
            .and_then(|value| value.checked_add(hlp_funding_debt))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let utilization_bps = utilization_bps(total_debt, side.reserves.cash_reserve as u128)?;
        let utilization_error_nad =
            utilization_error_nad(utilization_bps, self.config.irm.target_utilization_bps as u64)?;
        let borrow_apr_nad = instantaneous_rate_apr_nad(
            rate_at_target_nad,
            utilization_error_nad,
            self.config.irm.curve_steepness_nad as u128,
        )?;
        let daily_borrow_limit = self.daily_limit_for_side(asset, self.config.max_daily_borrow_bps)?;
        let daily_borrow_remaining = self
            .side(asset)
            .daily_borrow_bucket
            .remaining(daily_borrow_limit, slot)?;
        let base_price = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let spot_price_nad = match asset {
            MarketAsset::Base => base_price,
            MarketAsset::Quote => {
                require!(base_price > 0, ErrorCode::InvalidSettlementPrice);
                let inverse = (NAD as u128)
                    .checked_mul(NAD as u128)
                    .and_then(|value| value.checked_div(base_price as u128))
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(inverse).map_err(|_| ErrorCode::MarketMathOverflow)?
            }
        };

        Ok(LendingSidePreview {
            conservative_depth_nad,
            borrow_index_nad,
            rate_at_target_nad,
            borrow_apr_nad,
            utilization_bps,
            fixed_debt,
            isolated_debt,
            hlp_funding_debt,
            total_debt,
            daily_borrow_limit,
            daily_borrow_remaining,
            spot_price_nad,
        })
    }

    pub(crate) fn borrow_capacity_quote(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        projected_borrow_amount: Option<u64>,
        slot: u64,
    ) -> Result<BorrowCapacityQuote> {
        let debt_asset = collateral_asset.opposite();
        let collateral_side = self.side(collateral_asset);
        let debt_side = self.side(debt_asset);
        let risk = self.current_risk()?;
        let collateral_value_nad = self.collateral_value_nad(collateral_asset, collateral_amount, &risk)?;
        let max_debt_by_cash = debt_side.reserves.cash_reserve;
        let daily_limit = self.daily_limit_for_side(debt_asset, self.config.max_daily_borrow_bps)?;
        let max_debt_by_daily_limit = debt_side.daily_borrow_bucket.remaining(daily_limit, slot)?;
        let context = NewPositionPreviewContext {
            market: self,
            debt_asset,
            collateral_amount,
            risk: &risk,
            existing_total_debt_nad: self.total_fixed_debt_nad(debt_asset)?,
            current_aggregate_contribution: match debt_asset {
                MarketAsset::Base => self.debt.global_health_quote_contribution_for_base_debt,
                MarketAsset::Quote => self.debt.global_health_base_contribution_for_quote_debt,
            },
        };
        let max_debt_by_health = {
            let current_health = self.market_health_from_risk(&risk)?;
            if self.assert_market_health_snapshot(&current_health).is_err() {
                0
            } else {
                let mut low = 0_u64;
                let mut high = debt_side.reserves.live_reserve;
                while low < high {
                    let midpoint = low + (high - low) / 2 + 1;
                    let (terms, _) = context.terms(midpoint)?;
                    let accepted = terms.max_debt >= midpoint
                        && terms.projected_market_health_bps >= self.config.borrow_market_health_floor_bps as u64;
                    if accepted {
                        low = midpoint;
                    } else {
                        high = midpoint - 1;
                    }
                }
                low
            }
        };
        let max_debt = max_debt_by_health.min(max_debt_by_cash).min(max_debt_by_daily_limit);
        let projected_debt_amount = projected_borrow_amount.unwrap_or(max_debt);
        let (projected_terms, projected_global_health_contribution) = context.terms(projected_debt_amount)?;
        let projected_debt_nad = normalize_to_nad(projected_debt_amount as u128, debt_side.asset_decimals)?;
        let projected_health_bps = if projected_debt_nad == 0 {
            u64::MAX
        } else {
            health_bps(collateral_value_nad, projected_debt_nad)?
        };
        let liquidation_debt_per_collateral_price_nad =
            if collateral_amount == 0 || projected_debt_amount == 0 || projected_terms.liquidation_cf_bps == 0 {
                0
            } else {
                let collateral_nad = normalize_to_nad(collateral_amount as u128, collateral_side.asset_decimals)?;
                let debt_nad = normalize_to_nad(projected_debt_amount as u128, debt_side.asset_decimals)?;
                let price = ceil_div(
                    debt_nad
                        .checked_mul(BPS_DENOMINATOR as u128)
                        .and_then(|value| value.checked_mul(NAD as u128))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    collateral_nad
                        .checked_mul(projected_terms.liquidation_cf_bps as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                )
                .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow)?
            };

        Ok(BorrowCapacityQuote {
            collateral_value_nad,
            max_debt_by_health,
            max_debt_by_cash,
            max_debt_by_daily_limit,
            max_debt,
            projected_debt_amount,
            projected_health_bps,
            projected_terms,
            projected_global_health_contribution,
            liquidation_debt_per_collateral_price_nad,
        })
    }

    pub(crate) fn position_debt_side_quote(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
    ) -> Result<PositionDebtSideQuote> {
        let collateral_asset = debt_asset.opposite();
        let debt = match debt_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let collateral_amount = borrow_position.collateral(collateral_asset);
        let global_health_contribution = borrow_position.global_health_contribution(debt_asset);
        let liquidation_cf_bps = borrow_position.liquidation_cf_bps(debt_asset);
        let risk = self.current_risk()?;
        let collateral_value_nad = self.collateral_value_nad(collateral_asset, collateral_amount, &risk)?;
        let health_bps = if debt == 0 {
            u64::MAX
        } else {
            health_bps(
                collateral_value_nad,
                normalize_to_nad(debt, self.side(debt_asset).asset_decimals)?,
            )?
        };
        let liquidation_reference_price_nad = if debt == 0 {
            0
        } else {
            self.liquidation_reference_price_nad(borrow_position, debt_asset)?
        };
        let pricing = LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad: liquidation_reference_price_nad,
        };
        let liquidation_health_bps = if debt == 0 {
            u64::MAX
        } else {
            self.liquidation_health_bps_with_pricing(borrow_position, debt_asset, pricing)?
        };
        let terms = if debt == 0 {
            Default::default()
        } else {
            self.liquidation_terms_with_pricing(borrow_position, debt_asset, pricing)?
        };

        Ok(PositionDebtSideQuote {
            debt_asset,
            collateral_asset,
            fixed_debt: debt,
            collateral_amount,
            global_health_contribution,
            collateral_value_nad,
            health_bps,
            max_cf_bps: max_cf_bps_from_liquidation_cf(liquidation_cf_bps),
            liquidation_cf_bps,
            liquidation_reference_price_nad,
            liquidation_health_bps,
            is_liquidatable: self.is_position_liquidatable_with_risk(borrow_position, debt_asset, &risk)?,
            liquidation_incentive_bps: terms.liquidation_incentive_bps,
            insurance_funding_bps: terms.insurance_funding_bps,
            total_penalty_bps: terms.total_penalty_bps,
            max_repay_amount: terms.max_repay_amount,
        })
    }
}

pub(crate) struct NewPositionPreviewContext<'a> {
    pub(crate) market: &'a Market,
    pub(crate) debt_asset: MarketAsset,
    pub(crate) collateral_amount: u64,
    pub(crate) risk: &'a Risk,
    pub(crate) existing_total_debt_nad: u128,
    pub(crate) current_aggregate_contribution: u64,
}

impl NewPositionPreviewContext<'_> {
    pub(crate) fn terms(&self, projected_debt_amount: u64) -> Result<(DynamicBorrowTerms, u64)> {
        let debt_decimals = self.market.side(self.debt_asset).asset_decimals;
        let projected_debt_nad = normalize_to_nad(projected_debt_amount as u128, debt_decimals)?;
        let projected_total_debt_nad = self
            .existing_total_debt_nad
            .checked_add(projected_debt_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let contribution = self.market.debt_capped_global_health_contribution(
            self.debt_asset,
            projected_debt_amount as u128,
            self.collateral_amount,
            self.risk,
        )?;
        let projected_aggregate = self
            .current_aggregate_contribution
            .checked_add(contribution)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let terms = self.market.dynamic_borrow_terms(
            self.debt_asset,
            self.collateral_amount,
            self.existing_total_debt_nad,
            projected_total_debt_nad,
            projected_aggregate,
            self.risk,
        )?;
        Ok((terms, contribution))
    }
}
