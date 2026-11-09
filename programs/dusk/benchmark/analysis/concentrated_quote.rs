impl Market {
    pub(crate) fn quote_concentrated_integrated_with_fee(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        preliminary: PreliminarySwapInputs,
        protocol_fee_bps: u16,
    ) -> Result<Option<ConcentratedIntegratedAmmQuote>> {
        let (state, mark) = self.integrated_curve_state_with_mark_nad()?;
        self.evaluate_concentrated_swap_with_mark(
            asset_in,
            reserve_credit,
            preliminary,
            &state,
            protocol_fee_bps,
            true,
            mark,
        )
        .map(|(_, transition)| transition.map(|quote| *quote))
    }
}
