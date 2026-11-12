impl IntegratedCurveState {
    /// Decomposes total live reserves into the ordinary trader-facing tranche
    /// and two perfectly hedged hLP positions at the current reserve ratio.
    ///
    /// The Base hLP owns `base_hlp_equity` Base plus an equal Quote
    /// claim/debt; the Quote hLP is symmetric. Integer opposite claims are
    /// floored once and the same atoms are used for debt, so the residual
    /// opposite exposure is exactly zero rather than approximately zero.
    pub(crate) fn from_total_reserves(
        total_base: u128,
        total_quote: u128,
        base_hlp_equity: u128,
        quote_hlp_equity: u128,
    ) -> Result<Self> {
        require!(total_base > 0 && total_quote > 0, ErrorCode::InsufficientLiquidity);
        let quote_hlp_base_claim = mul_div_u128(quote_hlp_equity, total_base, total_quote)?;
        let base_hlp_quote_claim = mul_div_u128(base_hlp_equity, total_quote, total_base)?;
        let ordinary_base = total_base
            .checked_sub(base_hlp_equity)
            .and_then(|value| value.checked_sub(quote_hlp_base_claim))
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        let ordinary_quote = total_quote
            .checked_sub(quote_hlp_equity)
            .and_then(|value| value.checked_sub(base_hlp_quote_claim))
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        require!(
            ordinary_base > 0 && ordinary_quote > 0,
            ErrorCode::InsufficientLiquidity
        );
        Ok(Self {
            ordinary_base,
            ordinary_quote,
            base_hlp_equity,
            quote_hlp_equity,
            base_hlp_quote_debt: base_hlp_quote_claim,
            quote_hlp_base_debt: quote_hlp_base_claim,
            ..Self::default()
        })
    }
}
