pub(crate) use preview::NewPositionPreviewContext;

impl Market {
    /// Liquidation capacity divided by debt, in basis points, for protection
    /// orders. Uses the same linear collateral valuation as borrow liquidation.
    /// Debt-free sides return u64::MAX. Call update before observing this value.
    pub fn borrow_protection_health_bps(&self, position: &BorrowPosition, asset: MarketAsset) -> Result<u64> {
        self.borrow_protection_health_bps_with_credit(position, asset, position.collateral(asset.opposite()))
    }

    pub fn is_position_liquidatable(&self, borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> Result<bool> {
        self.is_position_liquidatable_with_risk_and_credit(
            borrow_position,
            debt_asset,
            borrow_position.collateral(debt_asset.opposite()),
            &self.current_risk()?,
        )
    }

    pub fn deposit_collateral(
        &mut self,
        borrow_position: &mut BorrowPosition,
        market_asset: MarketAsset,
        collateral_credit: u64,
    ) -> Result<CollateralReceipt> {
        self.deposit_collateral_with_fee(
            borrow_position,
            market_asset,
            collateral_credit,
            LeverageCollateralFee::default(),
        )
    }

    pub fn repay(
        &mut self,
        borrow_position: &mut BorrowPosition,
        repay_asset: MarketAsset,
        repay_credit: u64,
    ) -> Result<DebtReceipt> {
        self.repay_with_finalization_and_fee(
            borrow_position,
            repay_asset,
            repay_credit,
            LeverageCollateralFee::default(),
            None,
        )
    }

    pub(crate) fn is_position_liquidatable_with_risk(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        risk: &Risk,
    ) -> Result<bool> {
        self.is_position_liquidatable_with_risk_and_credit(
            borrow_position,
            debt_asset,
            borrow_position.collateral(debt_asset.opposite()),
            risk,
        )
    }

    pub(crate) fn liquidation_collateral_value_nad(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        risk: &Risk,
    ) -> Result<u128> {
        if collateral_amount == 0 {
            return Ok(0);
        }
        let collateral_amount_nad =
            self.normalize_amount(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        let (geometry, point, direction) = self
            .pessimistic_concentrated_curve(collateral_asset, risk, false)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        geometry
            .quote_exact_in(point, collateral_amount_nad, direction)
            .map(|quote| quote.amount_out)
    }
}
