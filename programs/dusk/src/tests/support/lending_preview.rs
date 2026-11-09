impl Market {
    #[cfg(test)]
    pub(crate) fn borrow_capacity_quote(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        collateral_fee: LeverageCollateralFee,
        projected_borrow_amount: Option<u64>,
        slot: u64,
    ) -> Result<BorrowCapacityQuote> {
        let fees = match collateral_asset {
            MarketAsset::Base => LendingCollateralFees {
                base: collateral_fee,
                ..Default::default()
            },
            MarketAsset::Quote => LendingCollateralFees {
                quote: collateral_fee,
                ..Default::default()
            },
        };
        self.borrow_capacity_quote_with_market_fees(
            collateral_asset,
            collateral_amount,
            fees,
            projected_borrow_amount,
            slot,
        )
    }

    #[cfg(test)]
    pub(crate) fn position_capacity_quote(
        &self,
        position: &BorrowPosition,
        collateral_asset: MarketAsset,
        projected_borrow_amount: Option<u64>,
        borrow_capacity: bool,
        slot: u64,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<PositionCapacityQuote> {
        let fees = match collateral_asset {
            MarketAsset::Base => LendingCollateralFees {
                base: collateral_fee,
                ..Default::default()
            },
            MarketAsset::Quote => LendingCollateralFees {
                quote: collateral_fee,
                ..Default::default()
            },
        };
        self.position_capacity_quote_with_market_fees(
            position,
            collateral_asset,
            projected_borrow_amount,
            borrow_capacity,
            slot,
            fees,
        )
    }
}
