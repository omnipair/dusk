use super::*;

const POSITION_CAPACITY_EXACT_SEARCH_LIMIT: u64 = 1 << 40;
const POSITION_CAPACITY_LARGE_SEARCH_STEPS: usize = 8;

fn position_capacity_search_steps(high: u64) -> usize {
    // Atom-exact search remains cheap for ordinary ranges. Above this bound,
    // previews return the last accepted lower bound so large raw-token markets
    // stay within SBF compute limits without overstating executable capacity.
    if high <= POSITION_CAPACITY_EXACT_SEARCH_LIMIT {
        u64::BITS as usize
    } else {
        POSITION_CAPACITY_LARGE_SEARCH_STEPS
    }
}

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
            MarketAsset::Base => self.normalize_amount(base_depth as u128, side.asset_decimals)?,
            MarketAsset::Quote => self.normalize_amount(quote_depth as u128, side.asset_decimals)?,
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
        let projected_debt_nad = self.normalize_amount(projected_debt_amount as u128, debt_side.asset_decimals)?;
        let projected_health_bps = if projected_debt_nad == 0 {
            u64::MAX
        } else {
            health_bps(collateral_value_nad, projected_debt_nad)?
        };
        let liquidation_debt_per_collateral_price_nad =
            if collateral_amount == 0 || projected_debt_amount == 0 || projected_terms.liquidation_cf_bps == 0 {
                0
            } else {
                let collateral_nad =
                    self.normalize_amount(collateral_amount as u128, collateral_side.asset_decimals)?;
                let debt_nad = self.normalize_amount(projected_debt_amount as u128, debt_side.asset_decimals)?;
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
                self.normalize_amount(debt, self.side(debt_asset).asset_decimals)?,
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
        let projected_debt_nad = self
            .market
            .normalize_amount(projected_debt_amount as u128, debt_decimals)?;
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

struct ExistingPositionCapacityContext<'a> {
    market: &'a Market,
    position: &'a BorrowPosition,
    debt_asset: MarketAsset,
    collateral_amount: u64,
    risk: &'a Risk,
    debt_index: u128,
    position_shares: u128,
    aggregate_shares: u128,
    external_debt_nad: u128,
    geometry: ConcentratedCurveGeometry,
    point: ConcentratedCurvePoint,
    direction: ConcentratedCurveDirection,
    collateral_amount_nad: u128,
    impact_value_nad: u128,
    collateral_reserve_nad: u128,
    debt_reserve_nad: u128,
}

impl ExistingPositionCapacityContext<'_> {
    fn projection(&self, borrow_amount: u64) -> Result<PositionBorrowProjection> {
        let debt_shares = if borrow_amount == 0 {
            0
        } else {
            Debt::debt_to_shares(borrow_amount, self.debt_index)?
        };
        let aggregate_debt_increase = self
            .market
            .debt
            .fixed_debt_increase_for_shares(self.debt_asset, debt_shares)?;
        let projected_position_debt = Debt::shares_to_debt(
            self.position_shares
                .checked_add(debt_shares)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            self.debt_index,
        )?;
        let projected_total_debt = Debt::shares_to_debt(
            self.aggregate_shares
                .checked_add(debt_shares)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            self.debt_index,
        )?;
        let target_contribution = self.market.debt_capped_global_health_contribution(
            self.debt_asset,
            projected_position_debt,
            self.collateral_amount,
            self.risk,
        )?;
        let projected_aggregate = self.market.projected_aggregate_global_health_contribution(
            self.position,
            self.debt_asset,
            target_contribution,
        )?;
        let projected_total_debt_nad = self
            .market
            .normalize_amount(projected_total_debt, self.market.side(self.debt_asset).asset_decimals)?;
        let (effective_existing_debt_nad, projected_market_health_bps) =
            self.market.global_side_health_with_virtual_reserves(
                self.debt_asset,
                self.external_debt_nad,
                projected_total_debt_nad,
                projected_aggregate,
                self.risk,
                self.collateral_reserve_nad,
                self.debt_reserve_nad,
            )?;
        let utilized = if effective_existing_debt_nad == 0 {
            0
        } else {
            self.geometry
                .quote_exact_out(self.point, effective_existing_debt_nad, self.direction)?
                .amount_in
        };
        let total_value = self
            .geometry
            .quote_exact_in(
                self.point,
                utilized
                    .checked_add(self.collateral_amount_nad)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                self.direction,
            )?
            .amount_out;
        let user_max_debt = total_value.saturating_sub(effective_existing_debt_nad);
        let base_cf_bps = if self.impact_value_nad == 0 {
            0
        } else {
            user_max_debt
                .saturating_mul(BPS_DENOMINATOR as u128)
                .checked_div(self.impact_value_nad)
                .unwrap_or(0)
        };
        let liquidation_cf_bps = base_cf_bps.min(MAX_COLLATERAL_FACTOR_BPS as u128) as u16;
        let max_cf_bps = max_cf_bps_from_liquidation_cf(liquidation_cf_bps);
        let max_debt_nad = self
            .impact_value_nad
            .saturating_mul(max_cf_bps as u128)
            .checked_div(BPS_DENOMINATOR as u128)
            .unwrap_or(0);
        let terms = DynamicBorrowTerms {
            max_debt: self
                .market
                .denormalize_amount_floor(max_debt_nad, self.market.side(self.debt_asset).asset_decimals)?,
            max_cf_bps,
            liquidation_cf_bps,
            effective_existing_debt_nad,
            projected_market_health_bps,
        };
        Ok(PositionBorrowProjection {
            debt_shares,
            aggregate_debt_increase,
            projected_position_debt,
            target_contribution,
            terms,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PositionCapacityQuote {
    pub existing_debt: u128,
    pub max_borrow_amount: Option<u64>,
    pub max_withdraw_amount: Option<u64>,
    pub projected_borrow_amount: u64,
    pub projected_debt_amount: u128,
    pub collateral_value_nad: u128,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
    pub liquidation_price_nad: u64,
    pub borrow_allowed: bool,
}

impl Market {
    pub(crate) fn position_capacity_quote(
        &self,
        position: &BorrowPosition,
        collateral_asset: MarketAsset,
        projected_borrow_amount: Option<u64>,
        borrow_capacity: bool,
        slot: u64,
    ) -> Result<PositionCapacityQuote> {
        let debt_asset = collateral_asset.opposite();
        let collateral_amount = position.collateral(collateral_asset);
        let debt_index = self.debt.borrow_index(debt_asset);
        let shares = match debt_asset {
            MarketAsset::Base => position.fixed_base_shares,
            MarketAsset::Quote => position.fixed_quote_shares,
        };
        let existing_debt = Debt::shares_to_debt(shares, debt_index)?;
        let requested_borrow_amount = projected_borrow_amount;
        let zero_draw = requested_borrow_amount.unwrap_or(0) == 0;
        if !borrow_capacity && zero_draw && existing_debt == 0 {
            return Ok(PositionCapacityQuote {
                existing_debt,
                max_borrow_amount: None,
                max_withdraw_amount: Some(collateral_amount),
                projected_borrow_amount: 0,
                projected_debt_amount: 0,
                collateral_value_nad: 0,
                max_cf_bps: 0,
                liquidation_cf_bps: 0,
                liquidation_price_nad: 0,
                borrow_allowed: true,
            });
        }
        let risk = &self.risk;
        let market_healthy = self
            .assert_market_health_snapshot(&self.market_health_from_risk(risk)?)
            .is_ok();
        let aggregate_shares = match debt_asset {
            MarketAsset::Base => self.debt.fixed_base_shares,
            MarketAsset::Quote => self.debt.fixed_quote_shares,
        };
        let external_debt_nad = self.external_fixed_debt_nad(position, debt_asset)?;
        let (geometry, point, direction) = self
            .pessimistic_borrow_cpmm(collateral_asset, risk, true)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let collateral_amount_nad =
            self.normalize_amount(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let impact_value_nad = geometry
            .quote_exact_in(point, collateral_amount_nad, direction)?
            .amount_out;
        let (collateral_reserve_nad, debt_reserve_nad) = match collateral_asset {
            MarketAsset::Base => (point.base_reserve, point.quote_reserve),
            MarketAsset::Quote => (point.quote_reserve, point.base_reserve),
        };
        let borrow_context = ExistingPositionCapacityContext {
            market: self,
            position,
            debt_asset,
            collateral_amount,
            risk,
            debt_index,
            position_shares: shares,
            aggregate_shares,
            external_debt_nad,
            geometry,
            point,
            direction,
            collateral_amount_nad,
            impact_value_nad,
            collateral_reserve_nad,
            debt_reserve_nad,
        };
        let daily_limit = self.daily_limit_for_side(debt_asset, self.config.max_daily_borrow_bps)?;
        let cash_limit = self.side(debt_asset).reserves.cash_reserve;
        let daily_remaining = self.side(debt_asset).daily_borrow_bucket.remaining(daily_limit, slot)?;
        let collateral_value_nad = borrow_context.impact_value_nad;
        let max_debt_nad_by_collateral = collateral_value_nad
            .checked_mul(max_cf_bps_from_liquidation_cf(MAX_COLLATERAL_FACTOR_BPS) as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let max_debt_by_collateral =
            self.denormalize_amount_floor(max_debt_nad_by_collateral, self.side(debt_asset).asset_decimals)?;
        let existing_debt_u64 = u64::try_from(existing_debt).unwrap_or(u64::MAX);
        let collateral_limited_headroom = max_debt_by_collateral.saturating_sub(existing_debt_u64);
        let upper = cash_limit
            .min(daily_remaining)
            .min(collateral_limited_headroom)
            .min(i64::MAX as u64);
        let mut low = 0;
        let mut high = if borrow_capacity && market_healthy && collateral_amount > 0 {
            upper
        } else {
            0
        };
        let search_steps = position_capacity_search_steps(high);
        let use_cached_borrow_projection = upper > POSITION_CAPACITY_EXACT_SEARCH_LIMIT;
        let mut steps = 0;
        while low < high {
            if steps == search_steps {
                break;
            }
            steps += 1;
            let midpoint = low + (high - low) / 2 + 1;
            let projection = if use_cached_borrow_projection {
                borrow_context.projection(midpoint)?
            } else {
                self.position_borrow_projection(position, debt_asset, midpoint, collateral_amount, risk)?
            };
            if projection.terms.max_debt as u128 >= projection.projected_position_debt
                && projection.terms.projected_market_health_bps >= self.config.borrow_market_health_floor_bps as u64
            {
                low = midpoint;
            } else {
                high = midpoint - 1;
            }
        }
        let max_borrow_amount = borrow_capacity.then_some(low);
        let projected_borrow_amount = requested_borrow_amount.unwrap_or(max_borrow_amount.unwrap_or(0));
        let projection = if use_cached_borrow_projection {
            borrow_context.projection(projected_borrow_amount)?
        } else {
            self.position_borrow_projection(position, debt_asset, projected_borrow_amount, collateral_amount, risk)?
        };
        let borrow_allowed = projected_borrow_amount == 0
            || (market_healthy
                && projected_borrow_amount <= upper
                && projection.terms.max_debt as u128 >= projection.projected_position_debt
                && projection.terms.projected_market_health_bps >= self.config.borrow_market_health_floor_bps as u64);
        // A deposit with no draw preserves the position's issued terms.
        let liquidation_cf_bps = if projected_borrow_amount == 0 {
            position.liquidation_cf_bps(debt_asset)
        } else {
            projection.terms.liquidation_cf_bps
        };
        let max_cf_bps = max_cf_bps_from_liquidation_cf(liquidation_cf_bps);
        let liquidation_price_nad =
            if collateral_amount == 0 || projection.projected_position_debt == 0 || liquidation_cf_bps == 0 {
                0
            } else {
                let collateral_nad =
                    self.normalize_amount(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
                let debt_nad =
                    self.normalize_amount(projection.projected_position_debt, self.side(debt_asset).asset_decimals)?;
                let price = ceil_div(
                    debt_nad
                        .checked_mul(BPS_DENOMINATOR as u128)
                        .and_then(|value| value.checked_mul(NAD as u128))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    collateral_nad
                        .checked_mul(liquidation_cf_bps as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                )
                .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow)?
            };
        let mut low = 0;
        let mut high = if borrow_capacity { 0 } else { collateral_amount };
        let search_steps = position_capacity_search_steps(high);
        let mut steps = 0;
        while low < high {
            if steps == search_steps {
                break;
            }
            steps += 1;
            let midpoint = low + (high - low) / 2 + 1;
            let withdrawal =
                self.position_withdrawal_projection(position, collateral_asset, collateral_amount - midpoint)?;
            if withdrawal.max_debt as u128 >= withdrawal.position_debt {
                low = midpoint;
            } else {
                high = midpoint - 1;
            }
        }
        Ok(PositionCapacityQuote {
            existing_debt,
            max_borrow_amount,
            max_withdraw_amount: (!borrow_capacity).then_some(low),
            projected_borrow_amount,
            projected_debt_amount: projection.projected_position_debt,
            collateral_value_nad,
            max_cf_bps,
            liquidation_cf_bps,
            liquidation_price_nad,
            borrow_allowed,
        })
    }
}
