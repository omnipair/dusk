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
        let (protocol_fee, _) = split_revenue(interest_credit, protocol_fee_bps)?;
        let lp_interest = self.record_interest_custody(
            interest_credit,
            protocol_fee,
            protocol_auction_split,
            referral_interest_amount,
        )?;
        self.fees.unallocated_interest_liability = self
            .fees
            .unallocated_interest_liability
            .checked_add(lp_interest)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.carry_forward_interest_with_supply(eligible_srlp_supply)?;
        self.fees.assert_backed()?;
        FeesReceipt::from_side(self)
    }
}

impl MarketSide {
    pub(crate) fn record_interest_custody(
        &mut self,
        interest_credit: u64,
        protocol_fee: u64,
        protocol_auction_split: ProtocolAuctionSplit,
        referral_interest_amount: u64,
    ) -> Result<u64> {
        let lp_interest = interest_credit
            .checked_sub(protocol_fee)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require_gte!(protocol_fee, referral_interest_amount, ErrorCode::FeeMathOverflow);
        let remaining_protocol_fee = protocol_fee
            .checked_sub(referral_interest_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let (fee_auction_amount, buyback_auction_amount) =
            split_protocol_auction_fee(remaining_protocol_fee, &protocol_auction_split)?;
        self.fees.interest_vault_balance = self
            .fees
            .interest_vault_balance
            .checked_add(interest_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.referral_interest_liability = self
            .fees
            .referral_interest_liability
            .checked_add(referral_interest_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_protocol_fee_liability = self
            .fees
            .interest_protocol_fee_liability
            .checked_add(fee_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_buyback_fee_liability = self
            .fees
            .interest_buyback_fee_liability
            .checked_add(buyback_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(lp_interest)
    }
}
