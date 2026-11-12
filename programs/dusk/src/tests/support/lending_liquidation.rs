fn liquidation_health_floor_bps(liquidation_cf_bps: u16) -> u64 {
    if liquidation_cf_bps == 0 {
        return u64::MAX;
    }
    ceil_div((BPS_DENOMINATOR as u128).pow(2), liquidation_cf_bps as u128)
        .unwrap_or(u128::from(u64::MAX))
        .min(u128::from(u64::MAX)) as u64
}

fn liquidation_max_incentive_bps(health_bps: u64, min_health_bps: u64) -> u16 {
    let shortfall = min_health_bps.saturating_sub(health_bps);
    let max_for_config = min_health_bps
        .saturating_sub(BPS_DENOMINATOR as u64 + 1)
        .min(LIQUIDATION_MAX_INCENTIVE_BPS as u64);
    shortfall.max(LIQUIDATION_INCENTIVE_BPS as u64).min(max_for_config) as u16
}

fn max_repay_to_restore_health_with_pricing(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    total_penalty_bps: u16,
    pricing: LiquidationPricing,
) -> Result<u64> {
    let debt_before = position_debt(market, borrow_position, debt_asset)?;
    let debt_decimals = market.side(debt_asset).asset_decimals;
    let debt_value_nad = market.normalize_amount(debt_before, debt_decimals)?;
    let collateral_value_nad = position_collateral_value_with_pricing_amount(
        market,
        debt_asset,
        position_collateral(borrow_position, debt_asset),
        pricing,
    )?;
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
    Ok(market
        .denormalize_amount_ceil(repay_value_nad, debt_decimals)?
        .min(u64::try_from(debt_before).unwrap_or(u64::MAX)))
}

impl Liquidation {
    pub fn new(debt_asset: MarketAsset, repay_credit: u64, max_socialized_loss: u64, terms: LiquidationTerms) -> Self {
        Self {
            debt_asset,
            repay_credit,
            max_socialized_loss,
            terms,
            pricing: LiquidationPricing::PessimisticReserves,
            collateral_fee: None,
            auction_eligibility_fee: None,
            exact_net_auction_pricing: false,
        }
    }
}

impl Market {
    pub fn liquidation_terms_with_pricing(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        pricing: LiquidationPricing,
    ) -> Result<LiquidationTerms> {
        self.liquidation_terms_with_pricing_and_credit(
            borrow_position,
            debt_asset,
            position_collateral(borrow_position, debt_asset),
            pricing,
        )
    }

    pub fn settle_liquidation(
        &mut self,
        borrow_position: &mut BorrowPosition,
        debt_asset: MarketAsset,
        repay_credit: u64,
        max_socialized_loss: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
    ) -> Result<LiquidationReceipt> {
        Liquidation::new_with_pricing(debt_asset, repay_credit, max_socialized_loss, terms, pricing)
            .apply(self, borrow_position)
    }
}
