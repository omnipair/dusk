impl Market {
    /// Auction reference prices net bidder and junior receipts. Each
    /// transfer is grossed up independently under the effective mint fee.
    pub fn settle_auction_liquidation_with_fees(
        &mut self,
        borrow_position: &mut BorrowPosition,
        debt_asset: MarketAsset,
        repay_credit: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
        effective_fee: LeverageCollateralFee,
        eligibility_fee: LeverageCollateralFee,
    ) -> Result<LiquidationReceipt> {
        let mut liquidation = Liquidation::new_with_pricing(debt_asset, repay_credit, 0, terms, pricing);
        liquidation.collateral_fee = Some(effective_fee);
        liquidation.auction_eligibility_fee = Some(eligibility_fee);
        liquidation.exact_net_auction_pricing = true;
        liquidation.apply(self, borrow_position)
    }
}
