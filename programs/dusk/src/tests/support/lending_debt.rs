impl Debt {
    pub fn writeoff_isolated_position(
        &mut self,
        asset: MarketAsset,
        position_shares: &mut u128,
        position_principal: &mut u128,
    ) -> Result<DebtWriteoff> {
        require!(*position_shares > 0, ErrorCode::DebtShareDivisionOverflow);
        let borrow_index_nad = self.borrow_index(asset);
        let debt_written_off = u64::try_from(Self::shares_to_debt(*position_shares, borrow_index_nad)?)
            .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let (aggregate_shares, aggregate_principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        require_gte!(*aggregate_shares, *position_shares, ErrorCode::DebtShareMathOverflow);
        let aggregate_debt_before = Self::shares_to_debt(*aggregate_shares, borrow_index_nad)?;
        let aggregate_debt_after = Self::shares_to_debt(
            aggregate_shares
                .checked_sub(*position_shares)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            borrow_index_nad,
        )?;
        let aggregate_debt_written_off = u64::try_from(
            aggregate_debt_before
                .checked_sub(aggregate_debt_after)
                .ok_or(ErrorCode::DebtMathOverflow)?,
        )
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let principal_written_off = u64::try_from(*position_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
        require_gte!(*aggregate_principal, principal_written_off, ErrorCode::DebtMathOverflow);
        *aggregate_shares = aggregate_shares
            .checked_sub(*position_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *aggregate_principal = aggregate_principal
            .checked_sub(principal_written_off)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let shares_written_off = *position_shares;
        *position_shares = 0;
        *position_principal = 0;
        if *aggregate_shares == 0 {
            *aggregate_principal = 0;
        }
        Ok(DebtWriteoff {
            shares_written_off,
            debt_written_off,
            aggregate_debt_written_off,
            principal_written_off,
        })
    }
}
