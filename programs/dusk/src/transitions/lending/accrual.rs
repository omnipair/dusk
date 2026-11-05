//! One shared index is limited before any debt, fee, or LP ledger is changed.
//! Excess unrepresentable interest is forgone, never carried into a later slot.
use super::*;

impl Market {
    pub(crate) fn check_borrow_income_capacity(&self, asset: MarketAsset, projected: &Debt) -> Result<()> {
        let (fixed, isolated, principal, funding) = match asset {
            MarketAsset::Base => (
                projected.fixed_base_debt()?,
                projected.isolated_debt(asset)?,
                [projected.fixed_base_principal, projected.isolated_base_principal],
                &self.quote_hlp_vault,
            ),
            MarketAsset::Quote => (
                projected.fixed_quote_debt()?,
                projected.isolated_debt(asset)?,
                [projected.fixed_quote_principal, projected.isolated_quote_principal],
                &self.base_hlp_vault,
            ),
        };
        require!(
            fixed <= u64::MAX as u128 && isolated <= u64::MAX as u128,
            ErrorCode::DebtMathOverflow
        );
        let public = fixed
            .saturating_sub(principal[0] as u128)
            .checked_add(isolated.saturating_sub(principal[1] as u128))
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let funding = Debt::shares_to_debt(funding.debt_shares, projected.borrow_index(asset))?
            .saturating_sub(funding.debt_principal as u128);
        let public = u64::try_from(public).map_err(|_| ErrorCode::DebtMathOverflow)?;
        let funding = u64::try_from(funding).map_err(|_| ErrorCode::DebtMathOverflow)?;
        require!(
            self.income_accrual_fits(asset, public, funding)?,
            ErrorCode::IncomeEpochNeedsRollover
        );
        Ok(())
    }

    /// Check all accounting consumers of an index. Physical repayments and
    /// individual debt buckets retain SPL's u64 atom domain; the aggregate
    /// virtual reserve is wider because it includes cash and receivables.
    #[inline(never)]
    fn accrual_index_fits(&self, asset: MarketAsset, index: u128) -> Result<(bool, [u128; 3])> {
        let (fixed, isolated, principal, funding) = match asset {
            MarketAsset::Base => (
                self.debt.fixed_base_shares,
                self.debt.isolated_base_shares,
                [self.debt.fixed_base_principal, self.debt.isolated_base_principal],
                &self.quote_hlp_vault,
            ),
            MarketAsset::Quote => (
                self.debt.fixed_quote_shares,
                self.debt.isolated_quote_shares,
                [self.debt.fixed_quote_principal, self.debt.isolated_quote_principal],
                &self.base_hlp_vault,
            ),
        };
        let fixed = Debt::shares_to_debt(fixed, index)?;
        let isolated = Debt::shares_to_debt(isolated, index)?;
        let funding_debt = Debt::shares_to_debt(funding.debt_shares, index)?;
        if fixed > u64::MAX as u128 || isolated > u64::MAX as u128 || funding_debt > u64::MAX as u128 {
            return Ok((false, [fixed, isolated, funding_debt]));
        }
        let public = fixed
            .saturating_sub(principal[0] as u128)
            .checked_add(isolated.saturating_sub(principal[1] as u128))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let funding = funding_debt.saturating_sub(funding.debt_principal as u128);
        let (Ok(public), Ok(funding)) = (u64::try_from(public), u64::try_from(funding)) else {
            return Ok((false, [fixed, isolated, funding_debt]));
        };
        Ok((
            self.income_accrual_fits(asset, public, funding)?,
            [fixed, isolated, funding_debt],
        ))
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
        let (candidate, next_remainder, clipped) = match desired {
            Ok((next, fraction)) if next <= ceiling => (next, fraction, false),
            Ok(_) => (ceiling, 0, true),
            Err(anchor_lang::error::Error::AnchorError(detail))
                if detail.error_code_number == u32::from(ErrorCode::MarketMathOverflow) =>
            {
                (ceiling, 0, true)
            }
            Err(error) => return Err(error),
        };
        let (fits, candidate_debt) = self.accrual_index_fits(asset, candidate)?;
        if fits {
            return Ok((candidate, if clipped { 0 } else { next_remainder }, candidate_debt));
        }
        if candidate == index {
            return Ok((index, 0, candidate_debt));
        }
        // Only a numerical boundary takes this path. Keep a certified lower
        // endpoint; rejected trial indexes never mutate any ownership ledger.
        let mut low = index;
        let (_, mut accepted_debt) = self.accrual_index_fits(asset, low)?;
        let mut high = candidate - 1;
        while low < high {
            let middle = low + (high - low) / 2 + 1;
            let (fits, debt) = self.accrual_index_fits(asset, middle)?;
            if fits {
                low = middle;
                accepted_debt = debt;
            } else {
                high = middle - 1;
            }
        }
        // No excess fractional growth survives a saturated checkpoint.
        Ok((low, 0, accepted_debt))
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
