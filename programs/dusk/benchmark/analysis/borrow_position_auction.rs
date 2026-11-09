const LIQUIDATION_RESERVATION_FEE_BPS: u128 = 20;

impl BorrowPosition {
    pub fn assert_liquidation_auction(&self, debt_asset: MarketAsset) -> Result<()> {
        require!(
            self.active_liquidation_auction_asset()? == Some(debt_asset),
            ErrorCode::PositionNotLiquidatable
        );
        Ok(())
    }

    pub fn liquidation_auction_expired(&self, now: i64) -> Result<bool> {
        require!(
            self.has_active_liquidation_auction(),
            ErrorCode::PositionNotLiquidatable
        );
        require!(now >= self.auction_start_time, ErrorCode::MarketMathOverflow);
        Ok(now.saturating_sub(self.auction_start_time)
            >= crate::constants::LIQUIDATION_AUCTION_DURATION_SECONDS)
    }

    /// Applies the remaining opening premium to the current liquidation reference.
    /// The stored start and floor prices define the premium, not a fixed fill quote.
    pub fn liquidation_auction_price_nad(&self, now: i64, current_reference_price_nad: u64) -> Result<u64> {
        require!(
            self.has_active_liquidation_auction(),
            ErrorCode::PositionNotLiquidatable
        );
        require!(now >= self.auction_start_time, ErrorCode::MarketMathOverflow);
        require!(current_reference_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        require!(
            self.auction_start_price_nad >= self.auction_floor_price_nad && self.auction_floor_price_nad > 0,
            ErrorCode::BrokenInvariant
        );
        let duration = crate::constants::LIQUIDATION_AUCTION_DURATION_SECONDS;
        let elapsed = now.saturating_sub(self.auction_start_time).min(duration);
        let remaining = duration.checked_sub(elapsed).ok_or(ErrorCode::MarketMathOverflow)?;
        let premium = self
            .auction_start_price_nad
            .checked_sub(self.auction_floor_price_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let premium_numerator = (premium as u128)
            .checked_mul(remaining as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let premium_denominator = (self.auction_floor_price_nad as u128)
            .checked_mul(duration as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let remaining_premium = crate::math::arithmetic::mul_div_ceil_u128(
            current_reference_price_nad as u128,
            premium_numerator,
            premium_denominator,
        )?;
        let price = (current_reference_price_nad as u128)
            .checked_add(remaining_premium)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(price.min(u64::MAX as u128) as u64)
    }

    /// Adds the fixed LP reservation fee to the current auction quote.
    pub fn liquidation_auction_bid_price_nad(&self, now: i64, current_reference_price_nad: u64) -> Result<u64> {
        let price = self.liquidation_auction_price_nad(now, current_reference_price_nad)? as u128;
        let reservation_fee = price
            .checked_mul(LIQUIDATION_RESERVATION_FEE_BPS)
            .and_then(|value| value.checked_div(crate::constants::BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let bid_price = price
            .checked_add(reservation_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(bid_price.min(u64::MAX as u128) as u64)
    }

    pub fn start_liquidation_auction(
        &mut self,
        debt_asset: MarketAsset,
        start_time: i64,
        start_price_nad: u64,
        floor_price_nad: u64,
    ) {
        self.auction_debt_asset = debt_asset.code();
        self.auction_start_time = start_time;
        self.auction_start_price_nad = start_price_nad;
        self.auction_floor_price_nad = floor_price_nad;
    }
}
