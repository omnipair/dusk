impl HlpVault {
    pub fn add_debt_shares(&mut self, shares: u128) -> Result<()> {
        self.debt_shares = self
            .debt_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        Ok(())
    }

    pub fn add_debt_principal(&mut self, amount: u64) -> Result<()> {
        self.debt_principal = self
            .debt_principal
            .checked_add(amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        Ok(())
    }

    pub fn checkpoint_yield_from_srlp(&mut self, base_side: &MarketSide, quote_side: &MarketSide) -> Result<()> {
        self.checkpoint_yield_from_srlp_shares(base_side, quote_side, self.srlp_shares)
    }
}

impl Market {
    /// Validate that the current hLP hedge state admits new capital without
    /// exposing rebalance-engine intermediates outside the hLP domain.
    pub fn assert_hlp_entry_available(&self, target_asset: MarketAsset) -> Result<()> {
        require!(!self.hlp_frozen(target_asset), ErrorCode::HlpSettlementUnavailable);
        let prices = current_hlp_curve_prices(self)?;
        let entry = current_hlp_entry_state_with_prices(self, target_asset, prices)?;
        require!(entry.disposition.admits_entry(), ErrorCode::HlpSettlementUnavailable);
        Ok(())
    }
}
impl HlpEntryDisposition {
    pub(crate) const fn admits_entry(self) -> bool {
        matches!(self, Self::Settled | Self::ControllerGranularityLimited)
    }
}
