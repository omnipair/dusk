impl Market {
    pub fn leverage_reference_equity_bps(
        &self,
        position: &LeveragePosition,
        collateral_credit: u64,
    ) -> Result<u128> {
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
}
