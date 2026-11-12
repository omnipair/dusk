impl Market {
    /// A zeroed market at the current layout, the only layout the program
    /// reads. Fixtures start here: `Market::default()` has version 0, which
    /// every amount conversion rejects.
    pub(crate) fn current_layout() -> Self {
        Self { version: crate::constants::MARKET_LAYOUT_VERSION, ..Self::default() }
    }

    /// Checkpoint all elapsed state under the old parameters, apply every
    /// family as one atomic change, enforce market health and available
    /// liquidity, and then advance the revision of each family that changed.
    /// Any failure leaves the market untouched.
    pub(crate) fn execute_parameter_updates(
        &mut self,
        updates: &[MarketParameterUpdate],
        current_slot: u64,
    ) -> Result<()> {
        self.execute_parameter_updates_inner(updates, current_slot)
    }
}
