impl MarketSide {
    pub fn record_swap_fee_credit(
        &mut self,
        fee_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<FeesReceipt> {
        self.record_claimable_swap_fees(
            fee_credit,
            0,
            protocol_fee_bps,
            protocol_auction_split,
            self.shares.srlp_supply,
        )
    }

    pub fn record_interest_credit(
        &mut self,
        interest_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        referral_interest_amount: u64,
    ) -> Result<FeesReceipt> {
        self.record_interest_credit_with_supply(
            interest_credit,
            protocol_fee_bps,
            protocol_auction_split,
            referral_interest_amount,
            self.shares.srlp_supply,
        )
    }
}

impl MarketSide {
    /// Records swap fees physically held in the reserve vault but excluded
    /// from executable reserves as explicit liabilities.
    ///
    /// The protocol split applies only to `base_fee_credit`.
    /// A distributed dynamic surcharge belongs entirely to srLPs; retained
    /// surcharge must stay in the reserve and must not be passed here.
    pub fn record_claimable_swap_fees(
        &mut self,
        base_fee_credit: u64,
        distributed_dynamic_surcharge_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        eligible_srlp_supply: u64,
    ) -> Result<FeesReceipt> {
        self.record_swap_fee_allocation(
            base_fee_credit,
            distributed_dynamic_surcharge_credit,
            0,
            0,
            protocol_fee_bps,
            protocol_auction_split,
            eligible_srlp_supply,
        )
    }

    /// Test helper: interest already paid in cash and credited to holders.
    /// The LP share arrives in reserve cash and is indexed over
    /// `eligible_srlp_supply` (junior first, then senior); the protocol share
    /// is booked as liabilities held in the vault outside cash. The curve
    /// reserve does not move.
    pub fn record_interest_credit_with_supply(
        &mut self,
        interest_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        referral_interest_amount: u64,
        eligible_srlp_supply: u64,
    ) -> Result<FeesReceipt> {
        if interest_credit == 0 {
            return FeesReceipt::from_side(self);
        }
        let protocol_fee = (u128::from(interest_credit) * u128::from(protocol_fee_bps) / 10_000) as u64;
        let lp_interest = interest_credit - protocol_fee;
        self.fees
            .credit_protocol_interest_revenue(protocol_fee, protocol_auction_split, referral_interest_amount)?;
        self.credit_reserve(lp_interest, true)?;
        self.fees.ledger.credited = self
            .fees
            .ledger
            .credited
            .checked_add(u128::from(lp_interest))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let senior = self.fees.junior.allocate(
            lp_interest,
            crate::transitions::tranches::TrancheRevenueSource::BorrowInterest,
        )?;
        if senior > 0 {
            let supply = self.fees.junior.senior_supply(eligible_srlp_supply)?;
            let (growth, carry) = distribute_growth_q64(senior, supply, self.fees.interest_growth_remainder_scaled)?;
            self.fees.interest_growth_index_q64 = self
                .fees
                .interest_growth_index_q64
                .checked_add(growth)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            self.fees.interest_growth_remainder_scaled = carry;
        }
        self.fees.assert_backed()?;
        FeesReceipt::from_side(self)
    }
}
