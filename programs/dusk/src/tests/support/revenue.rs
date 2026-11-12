impl Fees {
    pub fn total_liability(&self) -> Result<u64> {
        self.swap_fee_liability
            .checked_add(self.interest_liability)
            .and_then(|value| value.checked_add(self.unallocated_swap_fee_liability))
            .and_then(|value| value.checked_add(self.unallocated_interest_liability))
            .and_then(|value| value.checked_add(self.swap_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.swap_buyback_fee_liability))
            .and_then(|value| value.checked_add(self.interest_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.interest_buyback_fee_liability))
            .and_then(|value| value.checked_add(self.referral_interest_liability))
            .and_then(|value| value.checked_add(self.revenue_interest_liability))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}
