use anchor_lang::prelude::*;

use crate::{constants::NAD, errors::ErrorCode, shared::math::ceil_div, state::MarketAsset};

#[cfg(test)]
std::thread_local! {
    static SHARES_TO_DEBT_CALL_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Debt {
    pub fixed_base_shares: u128,
    pub fixed_quote_shares: u128,
    pub base_borrow_index_nad: u128,
    pub quote_borrow_index_nad: u128,
    pub base_rate_at_target_nad: u128,
    pub quote_rate_at_target_nad: u128,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_last_accrual_slot: u64,
    pub quote_last_accrual_slot: u64,
    // Debt tracking (r_debt)
    /// Aggregate outstanding *principal* (borrowed token amount, excluding
    /// accrued interest) backing fixed margin debt on each side. Accrued
    /// interest is `fixed_*_debt - fixed_*_principal`; tracked so interest can
    /// be routed to the interest vault (non-compounding) instead of
    /// compounding into reserves. Principal is a raw token-atom balance and is
    /// therefore bounded by the corresponding `u64` reserve custody domain.
    pub fixed_base_principal: u64,
    pub fixed_quote_principal: u64,
    /// Aggregate isolated leverage debt. This debt contributes to utilization
    /// and interest, but is intentionally not utilized as normal margin debt.
    /// Shares remain `u128`; raw principal remains in the token account's
    /// `u64` amount domain.
    pub isolated_base_shares: u128,
    pub isolated_quote_shares: u128,
    pub isolated_base_principal: u64,
    pub isolated_quote_principal: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtClearance {
    pub shares_burned: u128,
    /// Cash actually accepted for this clearance. This is the canonical
    /// aggregate debt delta, not the caller's maximum input.
    pub cash_repaid: u64,
    pub debt_reduced: u64,
    pub aggregate_debt_reduced: u64,
    pub principal_paid: u64,
    pub interest_paid: u64,
    pub remaining_debt: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtRepaymentQuote {
    pub shares_to_burn: u128,
    pub cash_repaid: u64,
    pub position_debt_reduced: u64,
    pub remaining_position_debt: u64,
}

impl DebtClearance {
    pub fn live_debit_for_cash_repay(&self) -> Result<u64> {
        self.aggregate_debt_reduced
            .checked_sub(self.principal_paid)
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtWriteoff {
    pub shares_written_off: u128,
    pub debt_written_off: u64,
    pub aggregate_debt_written_off: u64,
    pub principal_written_off: u64,
}

impl Debt {
    pub fn debt_to_shares(amount: u64, borrow_index_nad: u128) -> Result<u128> {
        require!(amount > 0, ErrorCode::AmountZero);
        ceil_div(
            (amount as u128)
                .checked_mul(NAD as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            borrow_index_nad,
        )
        .ok_or(ErrorCode::MarketMathOverflow.into())
    }

    pub fn shares_to_debt(shares: u128, borrow_index_nad: u128) -> Result<u128> {
        if shares == 0 {
            return Ok(0);
        }
        #[cfg(test)]
        SHARES_TO_DEBT_CALL_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        shares
            .checked_mul(borrow_index_nad)
            .and_then(|value| value.checked_div(NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }

    /// Returns the maximum share burn whose canonical aggregate debt delta is
    /// no greater than `max_repay_amount`.
    ///
    /// Debt is stored as one aggregate share bucket while positions own subsets
    /// of those shares. Therefore the only split-resistant cash charge is:
    ///
    /// `floor(aggregate_before * index) - floor(aggregate_after * index)`.
    ///
    /// Selecting shares with a ceil conversion and charging the caller's input
    /// can erase up to one indexed share of debt on every split repayment. The
    /// floor candidate below, plus the only possible adjacent candidate, makes
    /// the aggregate delta telescope exactly across any split sequence.
    pub fn repayment_for_max(
        position_shares: u128,
        aggregate_shares: u128,
        borrow_index_nad: u128,
        max_repay_amount: u64,
    ) -> Result<DebtRepaymentQuote> {
        require!(max_repay_amount > 0, ErrorCode::AmountZero);
        require!(borrow_index_nad >= NAD as u128, ErrorCode::DebtShareDivisionOverflow);
        require!(position_shares > 0, ErrorCode::InsufficientDebt);
        require_gte!(aggregate_shares, position_shares, ErrorCode::DebtShareMathOverflow);

        let aggregate_debt_before = Self::shares_to_debt(aggregate_shares, borrow_index_nad)?;
        let position_debt_before = Self::shares_to_debt(position_shares, borrow_index_nad)?;
        require!(position_debt_before > 0, ErrorCode::InsufficientDebt);

        let mut shares_to_burn = (max_repay_amount as u128)
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(borrow_index_nad))
            .ok_or(ErrorCode::DebtShareMathOverflow)?
            .min(position_shares);

        // Aggregate floor phases can make one additional share fit the same
        // raw-token maximum. Since index >= 1.0, no second adjacent share can
        // fit once the mathematical floor candidate has been tested.
        if shares_to_burn < position_shares {
            let adjacent = shares_to_burn.checked_add(1).ok_or(ErrorCode::DebtShareMathOverflow)?;
            let adjacent_delta = aggregate_debt_before
                .checked_sub(Self::shares_to_debt(
                    aggregate_shares
                        .checked_sub(adjacent)
                        .ok_or(ErrorCode::DebtShareMathOverflow)?,
                    borrow_index_nad,
                )?)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            if adjacent_delta <= max_repay_amount as u128 {
                shares_to_burn = adjacent;
            }
        }
        require!(shares_to_burn > 0, ErrorCode::DebtShareDivisionOverflow);

        let aggregate_debt_after = Self::shares_to_debt(
            aggregate_shares
                .checked_sub(shares_to_burn)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            borrow_index_nad,
        )?;
        let position_debt_after = Self::shares_to_debt(
            position_shares
                .checked_sub(shares_to_burn)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            borrow_index_nad,
        )?;
        let cash_repaid = aggregate_debt_before
            .checked_sub(aggregate_debt_after)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        require!(
            cash_repaid > 0 && cash_repaid <= max_repay_amount as u128,
            ErrorCode::DebtMathOverflow
        );
        let position_debt_reduced = position_debt_before
            .checked_sub(position_debt_after)
            .ok_or(ErrorCode::DebtMathOverflow)?;

        Ok(DebtRepaymentQuote {
            shares_to_burn,
            cash_repaid: u64::try_from(cash_repaid).map_err(|_| ErrorCode::DebtMathOverflow)?,
            position_debt_reduced: u64::try_from(position_debt_reduced).map_err(|_| ErrorCode::DebtMathOverflow)?,
            remaining_position_debt: u64::try_from(position_debt_after).map_err(|_| ErrorCode::DebtMathOverflow)?,
        })
    }

    pub fn aggregate_debt_reduction_for_shares(
        aggregate_shares: u128,
        shares_to_burn: u128,
        borrow_index_nad: u128,
    ) -> Result<u64> {
        require!(shares_to_burn > 0, ErrorCode::DebtShareDivisionOverflow);
        require_gte!(aggregate_shares, shares_to_burn, ErrorCode::DebtShareMathOverflow);
        let debt_before = Self::shares_to_debt(aggregate_shares, borrow_index_nad)?;
        let debt_after = Self::shares_to_debt(
            aggregate_shares
                .checked_sub(shares_to_burn)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            borrow_index_nad,
        )?;
        u64::try_from(debt_before.checked_sub(debt_after).ok_or(ErrorCode::DebtMathOverflow)?)
            .map_err(|_| ErrorCode::DebtMathOverflow.into())
    }

    pub fn isolated_repayment_for_max(
        &self,
        asset: MarketAsset,
        position_shares: u128,
        max_repay_amount: u64,
    ) -> Result<DebtRepaymentQuote> {
        let aggregate_shares = match asset {
            MarketAsset::Base => self.isolated_base_shares,
            MarketAsset::Quote => self.isolated_quote_shares,
        };
        Self::repayment_for_max(
            position_shares,
            aggregate_shares,
            self.borrow_index(asset),
            max_repay_amount,
        )
    }

    #[cfg(test)]
    pub(crate) fn reset_shares_to_debt_call_count() {
        SHARES_TO_DEBT_CALL_COUNT.with(|count| count.set(0));
    }

    #[cfg(test)]
    pub(crate) fn shares_to_debt_call_count() -> usize {
        SHARES_TO_DEBT_CALL_COUNT.with(std::cell::Cell::get)
    }

    /// Increase tracked margin principal when new fixed margin debt is taken on.
    pub fn add_margin_principal(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let principal = match asset {
            MarketAsset::Base => &mut self.fixed_base_principal,
            MarketAsset::Quote => &mut self.fixed_quote_principal,
        };
        *principal = principal.checked_add(amount).ok_or(ErrorCode::DebtMathOverflow)?;
        Ok(())
    }

    pub fn add_isolated_debt(&mut self, asset: MarketAsset, amount: u64) -> Result<u128> {
        let borrow_index_nad = self.borrow_index(asset);
        let shares = Self::debt_to_shares(amount, borrow_index_nad)?;
        let (aggregate_shares, principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        let next_aggregate_shares = aggregate_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        let next_principal = principal.checked_add(amount).ok_or(ErrorCode::DebtMathOverflow)?;
        *aggregate_shares = next_aggregate_shares;
        *principal = next_principal;
        Ok(shares)
    }

    pub fn isolated_debt(&self, asset: MarketAsset) -> Result<u128> {
        let (shares, index) = match asset {
            MarketAsset::Base => (self.isolated_base_shares, self.base_borrow_index_nad),
            MarketAsset::Quote => (self.isolated_quote_shares, self.quote_borrow_index_nad),
        };
        Self::shares_to_debt(shares, index)
    }

    pub fn borrow_index(&self, asset: MarketAsset) -> u128 {
        match asset {
            MarketAsset::Base => self.base_borrow_index_nad,
            MarketAsset::Quote => self.quote_borrow_index_nad,
        }
    }

    pub fn clear_isolated_debt(
        &mut self,
        asset: MarketAsset,
        position_shares: &mut u128,
        position_principal: &mut u128,
        max_repay_amount: u64,
    ) -> Result<DebtClearance> {
        let repayment = self.isolated_repayment_for_max(asset, *position_shares, max_repay_amount)?;
        let shares_burned = repayment.shares_to_burn;
        let current_debt_u128 = Self::shares_to_debt(*position_shares, self.borrow_index(asset))?;
        let remaining_shares = position_shares
            .checked_sub(shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        let remaining_debt = repayment.remaining_position_debt;
        let debt_reduced = repayment.position_debt_reduced;

        let aggregate_shares_before = match asset {
            MarketAsset::Base => self.isolated_base_shares,
            MarketAsset::Quote => self.isolated_quote_shares,
        };
        let aggregate_debt_before = Self::shares_to_debt(aggregate_shares_before, self.borrow_index(asset))?;
        let aggregate_debt_reduced = repayment.cash_repaid;

        let (aggregate_shares, aggregate_principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        let position_principal_raw = u64::try_from(*position_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
        require_gte!(
            *aggregate_principal,
            position_principal_raw,
            ErrorCode::DebtMathOverflow
        );
        let position_principal_before = (*position_principal).min(current_debt_u128);
        let (position_principal_reduced, _) =
            crate::math::realized_interest_split(debt_reduced, current_debt_u128, position_principal_before)?;
        let aggregate_principal_before = u128::from(*aggregate_principal).min(aggregate_debt_before);
        let (principal_paid, interest_paid) = crate::math::realized_interest_split(
            aggregate_debt_reduced,
            aggregate_debt_before,
            aggregate_principal_before,
        )?;
        *position_shares = remaining_shares;
        *aggregate_shares = aggregate_shares
            .checked_sub(shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *position_principal = position_principal
            .checked_sub(position_principal_reduced as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        // Aggregate isolated principal is the exact sum of its position
        // principals. The canonical aggregate cash delta can have a different
        // floor phase from the position-local debt delta, so `principal_paid`
        // remains the cash/interest classification but must not mutate this
        // ownership ledger.
        *aggregate_principal = aggregate_principal
            .checked_sub(position_principal_reduced)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        if *position_shares == 0 {
            *position_principal = 0;
        }
        if *aggregate_shares == 0 {
            *aggregate_principal = 0;
        }

        Ok(DebtClearance {
            shares_burned,
            cash_repaid: repayment.cash_repaid,
            debt_reduced,
            aggregate_debt_reduced,
            principal_paid,
            interest_paid,
            remaining_debt,
        })
    }

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

    /// Reduce tracked margin principal for a cash-backed fixed-debt repayment,
    /// returning the realized *interest* portion (the non-compounding interest
    /// the caller should route to the interest vault). Uses the side's blended
    /// principal/debt ratio, which is aggregate-conservative across positions.
    pub fn realize_margin_repay(&mut self, asset: MarketAsset, repaid: u64) -> Result<u64> {
        self.realize_margin_clearance(asset, repaid, repaid)
    }

    /// Reduce tracked margin principal for a liquidation where only part of the
    /// cleared debt may be cash-backed. The returned interest is only the portion
    /// backed by `cash_repaid`; written-off interest is never treated as received.
    pub fn realize_margin_liquidation(
        &mut self,
        asset: MarketAsset,
        cash_repaid: u64,
        debt_reduction: u64,
    ) -> Result<u64> {
        self.realize_margin_clearance(asset, cash_repaid, debt_reduction)
    }

    fn realize_margin_clearance(&mut self, asset: MarketAsset, cash_repaid: u64, debt_reduction: u64) -> Result<u64> {
        require!(
            (cash_repaid as u128) <= debt_reduction as u128,
            ErrorCode::MarketMathOverflow
        );
        let fixed_debt = match asset {
            MarketAsset::Base => self.fixed_base_debt()?,
            MarketAsset::Quote => self.fixed_quote_debt()?,
        };
        let principal = match asset {
            MarketAsset::Base => u128::from(self.fixed_base_principal),
            MarketAsset::Quote => u128::from(self.fixed_quote_principal),
        }
        // Clamp guards against rounding making principal momentarily exceed debt.
        .min(fixed_debt);
        let (_, interest_paid) = crate::math::realized_interest_split(cash_repaid, fixed_debt, principal)?;
        let (principal_reduced, _) = crate::math::realized_interest_split(debt_reduction, fixed_debt, principal)?;
        let principal_slot = match asset {
            MarketAsset::Base => &mut self.fixed_base_principal,
            MarketAsset::Quote => &mut self.fixed_quote_principal,
        };
        *principal_slot = principal_slot.saturating_sub(principal_reduced);
        Ok(interest_paid)
    }

    pub fn fixed_base_debt(&self) -> Result<u128> {
        Self::shares_to_debt(self.fixed_base_shares, self.base_borrow_index_nad)
    }

    pub fn fixed_quote_debt(&self) -> Result<u128> {
        Self::shares_to_debt(self.fixed_quote_shares, self.quote_borrow_index_nad)
    }

    pub fn fixed_debt_increase_for_shares(&self, asset: MarketAsset, shares_added: u128) -> Result<u64> {
        let (shares_before, index_nad) = match asset {
            MarketAsset::Base => (self.fixed_base_shares, self.base_borrow_index_nad),
            MarketAsset::Quote => (self.fixed_quote_shares, self.quote_borrow_index_nad),
        };
        let debt_before = Self::shares_to_debt(shares_before, index_nad)?;
        let debt_after = Self::shares_to_debt(
            shares_before
                .checked_add(shares_added)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            index_nad,
        )?;
        u64::try_from(debt_after.checked_sub(debt_before).ok_or(ErrorCode::DebtMathOverflow)?)
            .map_err(|_| ErrorCode::DebtMathOverflow.into())
    }

    pub fn fixed_debt_reduction_for_shares(&self, asset: MarketAsset, shares_burned: u128) -> Result<u64> {
        let (shares_before, index_nad) = match asset {
            MarketAsset::Base => (self.fixed_base_shares, self.base_borrow_index_nad),
            MarketAsset::Quote => (self.fixed_quote_shares, self.quote_borrow_index_nad),
        };
        Self::aggregate_debt_reduction_for_shares(shares_before, shares_burned, index_nad)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/debt.rs");
}
