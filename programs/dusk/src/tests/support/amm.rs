impl Market {
    /// Finalizes observations after the complete swap and hLP settlement.
    /// The caller supplies the final executable price, including compounding.
    #[cfg(test)]
    pub(crate) fn finalize_amm_trade(
        &mut self,
        trade_start_price_nad: u64,
        final_price_nad: u64,
        current_slot: u64,
    ) -> Result<()> {
        self.checkpoint_amm_neutral_inventory_raw(current_slot, None)?;
        self.finalize_amm_trade_after_inventory_checkpoint(trade_start_price_nad, final_price_nad, current_slot)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn quote_integrated_concentrated_exact_in_nad(
        &self,
        gross_amount_in_nad: u128,
        frozen_total_fee_nad: u128,
        asset_in: MarketAsset,
    ) -> Result<Option<IntegratedFrozenFeeQuote>> {
        let Some(geometry) = self.current_concentrated_curve_geometry()? else {
            return Ok(None);
        };
        quote_integrated_exact_in_with_frozen_fee(
            self.integrated_curve_state_nad()?,
            geometry,
            gross_amount_in_nad,
            frozen_total_fee_nad,
            match asset_in {
                MarketAsset::Base => IntegratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => IntegratedSwapDirection::QuoteToBase,
            },
        )
        .map(Some)
    }

    #[cfg(test)]
    pub(crate) fn preliminary_swap_inputs_for_state(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
        pre_state: DynamicFeePreState,
    ) -> Result<PreliminarySwapInputs> {
        self.preliminary_swap_inputs_for_state_at_time(
            asset_in,
            reserve_credit,
            current_slot,
            self.config.start_time,
            pre_state,
        )
    }
}
