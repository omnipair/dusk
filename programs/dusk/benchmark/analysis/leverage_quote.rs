impl Market {
    pub fn quote_leverage_swap_at_time(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<LeverageSwapQuote> {
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        let preliminary = self.preliminary_swap_inputs_for_state_at_time(
            asset_in,
            amount_in,
            current_slot,
            current_unix_timestamp,
            pre_state,
        )?;
        let quote = self
            .quote_concentrated_integrated_with_fee(asset_in, amount_in, preliminary, 0)?
            .ok_or(ErrorCode::BrokenInvariant)?
            .as_swap_quote(asset_in);
        Ok(LeverageSwapQuote::from_amm(quote, current_slot))
    }
}
