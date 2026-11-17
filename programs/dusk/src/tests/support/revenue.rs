impl Fees {
    pub fn total_liability(&self) -> Result<u64> {
        let credited = u64::try_from(self.ledger.credited).map_err(|_| ErrorCode::MarketMathOverflow)?;
        self.swap_fee_liability
            .checked_add(credited)
            .and_then(|value| value.checked_add(self.unallocated_swap_fee_liability))
            .and_then(|value| value.checked_add(self.swap_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.swap_buyback_fee_liability))
            .and_then(|value| value.checked_add(self.interest_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.interest_buyback_fee_liability))
            .and_then(|value| value.checked_add(self.referral_interest_liability))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}
