//! One shared index is limited before any debt or interest ledger changes.
//! The only limit is the u64 atom domain of each debt bucket; interest is
//! never forgone for bookkeeping reasons.
use super::*;

impl Market {
    /// New borrowing keeps every fixed and isolated debt bucket in the u64
    /// atom domain that settlement and the accrual clip rely on.
    pub(crate) fn check_borrow_debt_domain(&self, asset: MarketAsset, projected: &Debt) -> Result<()> {
        let (fixed, isolated) = match asset {
            MarketAsset::Base => (projected.fixed_base_debt()?, projected.isolated_debt(asset)?),
            MarketAsset::Quote => (projected.fixed_quote_debt()?, projected.isolated_debt(asset)?),
        };
        require!(
            fixed <= u64::MAX as u128 && isolated <= u64::MAX as u128,
            ErrorCode::DebtMathOverflow
        );
        Ok(())
    }

    #[inline(never)]
    pub(super) fn bounded_accrual_index(
        &self,
        asset: MarketAsset,
        integrated_apr: u128,
    ) -> Result<(u128, u128, [u128; 3])> {
        let (index, remainder, shares) = match asset {
            MarketAsset::Base => (
                self.debt.base_borrow_index_nad,
                self.debt.base_borrow_index_remainder,
                [
                    self.debt.fixed_base_shares,
                    self.debt.isolated_base_shares,
                    self.quote_hlp_vault.debt_shares,
                ],
            ),
            MarketAsset::Quote => (
                self.debt.quote_borrow_index_nad,
                self.debt.quote_borrow_index_remainder,
                [
                    self.debt.fixed_quote_shares,
                    self.debt.isolated_quote_shares,
                    self.base_hlp_vault.debt_shares,
                ],
            ),
        };
        let denominator = NAD as u128 * MS_PER_YEAR as u128 * BPS_DENOMINATOR as u128;
        require!(
            index >= NAD as u128 && remainder < denominator,
            ErrorCode::BrokenInvariant
        );
        // floor(shares * index / NAD) <= u64::MAX. The exclusive bound fits
        // u128, so this is computed before attempting an unbounded product.
        let exclusive = (u64::MAX as u128 + 1) * NAD as u128;
        let ceiling = shares
            .into_iter()
            .filter(|s| *s != 0)
            .map(|s| (exclusive - 1) / s)
            .min()
            .ok_or(ErrorCode::BrokenInvariant)?;
        require_gte!(ceiling, index, ErrorCode::BrokenInvariant);
        let desired = self.unbounded_accrual_index(index, remainder, integrated_apr);
        // No excess fractional growth survives a clipped checkpoint.
        let (next, next_remainder) = match desired {
            Ok((next, fraction)) if next <= ceiling => (next, fraction),
            Ok(_) => (ceiling, 0),
            Err(anchor_lang::error::Error::AnchorError(detail))
                if detail.error_code_number == u32::from(ErrorCode::MarketMathOverflow) =>
            {
                (ceiling, 0)
            }
            Err(error) => return Err(error),
        };
        let debt = [
            Debt::shares_to_debt(shares[0], next)?,
            Debt::shares_to_debt(shares[1], next)?,
            Debt::shares_to_debt(shares[2], next)?,
        ];
        Ok((next, next_remainder, debt))
    }

    fn unbounded_accrual_index(&self, index: u128, remainder: u128, integrated_apr: u128) -> Result<(u128, u128)> {
        let base_denominator = NAD as u128 * MS_PER_YEAR as u128;
        let (base_whole, base_fraction) = mul_div_rem_u128(index, integrated_apr, base_denominator)?;
        let markup = self.protocol_interest_markup_bps as u128;
        let (markup_whole, markup_fraction) = mul_div_rem_u128(base_whole, markup, BPS_DENOMINATOR as u128)?;
        let denominator = base_denominator * BPS_DENOMINATOR as u128;
        let fraction = markup_fraction
            .checked_mul(base_denominator)
            .and_then(|v| {
                base_fraction
                    .checked_mul(BPS_DENOMINATOR as u128 + markup)
                    .and_then(|w| v.checked_add(w))
            })
            .and_then(|v| v.checked_add(remainder))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let next = index
            .checked_add(base_whole)
            .and_then(|v| v.checked_add(markup_whole))
            .and_then(|v| v.checked_add(fraction / denominator))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok((next, fraction % denominator))
    }
}
