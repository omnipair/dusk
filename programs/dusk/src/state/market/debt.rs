use anchor_lang::prelude::*;

use crate::{constants::NAD, errors::ErrorCode, shared::math::ceil_div, state::MarketAsset};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Debt {
    pub fixed_base_shares: u128,
    pub fixed_quote_shares: u128,
    pub base_borrow_index_nad: u128,
    pub quote_borrow_index_nad: u128,
    pub base_rate_at_target_nad: u128,
    pub quote_rate_at_target_nad: u128,
    pub utilized_base_collateral_for_quote_debt: u64,
    pub utilized_quote_collateral_for_base_debt: u64,
    pub last_accrual_slot: u64,
    // Debt tracking (r_debt)
    /// Aggregate outstanding *principal* (borrowed token amount, excluding
    /// accrued interest) backing fixed margin debt on each side. Accrued
    /// interest is `fixed_*_debt - fixed_*_principal`; tracked so interest can
    /// be routed to the interest vault (non-compounding) instead of
    /// compounding into reserves.
    pub fixed_base_principal: u128,
    pub fixed_quote_principal: u128,
    /// Aggregate isolated leverage debt. This debt contributes to utilization
    /// and interest, but is intentionally not utilized as normal margin debt.
    pub isolated_base_shares: u128,
    pub isolated_quote_shares: u128,
    pub isolated_base_principal: u128,
    pub isolated_quote_principal: u128,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtClearance {
    pub shares_burned: u128,
    pub debt_reduced: u64,
    pub principal_paid: u64,
    pub interest_paid: u64,
    pub remaining_debt: u64,
}

impl DebtClearance {
    pub fn live_debit_for_cash_repay(&self) -> Result<u64> {
        self.debt_reduced
            .checked_sub(self.principal_paid)
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtWriteoff {
    pub shares_written_off: u128,
    pub debt_written_off: u64,
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
        shares
            .checked_mul(borrow_index_nad)
            .and_then(|value| value.checked_div(NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }

    /// Increase tracked margin principal when new fixed margin debt is taken on.
    pub fn add_margin_principal(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let principal = match asset {
            MarketAsset::Base => &mut self.fixed_base_principal,
            MarketAsset::Quote => &mut self.fixed_quote_principal,
        };
        *principal = principal
            .checked_add(amount as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    pub fn add_isolated_debt(&mut self, asset: MarketAsset, amount: u64) -> Result<u128> {
        let borrow_index_nad = self.borrow_index(asset);
        let shares = Self::debt_to_shares(amount, borrow_index_nad)?;
        let (aggregate_shares, principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        *aggregate_shares = aggregate_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *principal = principal
            .checked_add(amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
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
        repay_amount: u64,
    ) -> Result<DebtClearance> {
        require!(repay_amount > 0, ErrorCode::AmountZero);
        let current_debt_u128 = Self::shares_to_debt(*position_shares, self.borrow_index(asset))?;
        require_gte!(current_debt_u128, repay_amount as u128, ErrorCode::InsufficientDebt);
        let current_debt = u64::try_from(current_debt_u128).map_err(|_| ErrorCode::DebtMathOverflow)?;
        let shares_burned = if repay_amount == current_debt {
            *position_shares
        } else {
            Self::debt_to_shares(repay_amount, self.borrow_index(asset))?.min(*position_shares)
        };
        require!(shares_burned > 0, ErrorCode::DebtShareDivisionOverflow);
        let remaining_shares = position_shares
            .checked_sub(shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        let remaining_debt_u128 = Self::shares_to_debt(remaining_shares, self.borrow_index(asset))?;
        let debt_reduced_u128 = current_debt_u128
            .checked_sub(remaining_debt_u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let debt_reduced = u64::try_from(debt_reduced_u128).map_err(|_| ErrorCode::DebtMathOverflow)?;

        let principal = (*position_principal).min(current_debt_u128);
        let (principal_paid, interest_paid) =
            crate::math::realized_interest_split(repay_amount, current_debt_u128, principal)?;
        let (principal_reduced, _) = crate::math::realized_interest_split(debt_reduced, current_debt_u128, principal)?;
        let (aggregate_shares, aggregate_principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        *position_shares = remaining_shares;
        *aggregate_shares = aggregate_shares
            .checked_sub(shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *position_principal = position_principal.saturating_sub(principal_reduced as u128);
        *aggregate_principal = aggregate_principal.saturating_sub(principal_reduced as u128);
        if *position_shares == 0 {
            *position_principal = 0;
        }
        if *aggregate_shares == 0 {
            *aggregate_principal = 0;
        }

        Ok(DebtClearance {
            shares_burned,
            debt_reduced,
            principal_paid,
            interest_paid,
            remaining_debt: u64::try_from(remaining_debt_u128).map_err(|_| ErrorCode::DebtMathOverflow)?,
        })
    }

    pub fn writeoff_isolated_position(
        &mut self,
        asset: MarketAsset,
        position_shares: &mut u128,
        position_principal: &mut u128,
    ) -> Result<DebtWriteoff> {
        require!(*position_shares > 0, ErrorCode::DebtShareDivisionOverflow);
        let debt_written_off = u64::try_from(Self::shares_to_debt(*position_shares, self.borrow_index(asset))?)
            .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let (aggregate_shares, aggregate_principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        require_gte!(*aggregate_shares, *position_shares, ErrorCode::DebtShareMathOverflow);
        let principal_written_off = u64::try_from(*position_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
        *aggregate_shares = aggregate_shares
            .checked_sub(*position_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *aggregate_principal = aggregate_principal.saturating_sub(*position_principal);
        let shares_written_off = *position_shares;
        *position_shares = 0;
        *position_principal = 0;
        if *aggregate_shares == 0 {
            *aggregate_principal = 0;
        }
        Ok(DebtWriteoff {
            shares_written_off,
            debt_written_off,
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
            MarketAsset::Base => self.fixed_base_principal,
            MarketAsset::Quote => self.fixed_quote_principal,
        }
        // Clamp guards against rounding making principal momentarily exceed debt.
        .min(fixed_debt);
        let (_, interest_paid) = crate::math::realized_interest_split(cash_repaid, fixed_debt, principal)?;
        let (principal_reduced, _) = crate::math::realized_interest_split(debt_reduction, fixed_debt, principal)?;
        let principal_slot = match asset {
            MarketAsset::Base => &mut self.fixed_base_principal,
            MarketAsset::Quote => &mut self.fixed_quote_principal,
        };
        *principal_slot = principal_slot.saturating_sub(principal_reduced as u128);
        Ok(interest_paid)
    }

    pub fn fixed_base_debt(&self) -> Result<u128> {
        Self::shares_to_debt(self.fixed_base_shares, self.base_borrow_index_nad)
    }

    pub fn fixed_quote_debt(&self) -> Result<u128> {
        Self::shares_to_debt(self.fixed_quote_shares, self.quote_borrow_index_nad)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/debt.rs");
}
