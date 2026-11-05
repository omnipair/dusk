//! Interest fee amounts are frozen on accrual, while the current recipient earns
//! operator cash on collection. Former recipients retain collected cash only.
use crate::{constants::BPS_DENOMINATOR, errors::ErrorCode, math::mul_div_u128};
use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct InterestRevenueBook {
    pub gross_unpaid: u64,
    pub protocol_unpaid: u64,
    pub operator_unpaid: u64,
    pub pending_gross: u64,
    pub pending_protocol_gross: u64,
    pub operator_claimable: u64,
    pub protocol_fraction_q64: u64,
    pub operator_fraction_q64: u64,
    pub protocol_cash_fraction_q64: u64,
}
impl Space for Box<InterestRevenueBook> {
    const INIT_SPACE: usize = InterestRevenueBook::INIT_SPACE;
}

/// Carry fractional atoms across changing denominators without floating point.
/// A fraction never creates a claim until sufficient measured cash is present.
fn split_with_fraction(amount: u64, numerator: u64, denominator: u64, fraction: &mut u64) -> Result<u64> {
    require!(denominator > 0 && numerator <= denominator, ErrorCode::BrokenInvariant);
    let product = amount as u128 * numerator as u128;
    let whole = product / denominator as u128;
    let fractional = ((product % denominator as u128) << 64) / denominator as u128 + *fraction as u128;
    *fraction = fractional as u64;
    u64::try_from(whole + (fractional >> 64)).map_err(|_| ErrorCode::MarketMathOverflow.into())
}
fn proportional(amount: u64, part: u64, total: u64) -> Result<u64> {
    if total == 0 {
        require_eq!(part, 0, ErrorCode::BrokenInvariant);
        return Ok(0);
    }
    require_gte!(total, part, ErrorCode::BrokenInvariant);
    u64::try_from(mul_div_u128(amount as u128, part as u128, total as u128)?)
        .map_err(|_| ErrorCode::MarketMathOverflow.into())
}
impl InterestRevenueBook {
    pub fn unpaid_fees(&self) -> Result<u64> {
        self.protocol_unpaid
            .checked_add(self.operator_unpaid)
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }
    /// Repayments/defaults clear proportional historical fees. A default records
    /// no payable cash; token transfers later supply their measured net credit.
    pub fn synchronize(&mut self, gross: u64, paid: u64, markup: u16, operator_share: u16) -> Result<u64> {
        require!(
            markup <= BPS_DENOMINATOR && operator_share <= crate::state::MAX_OPERATOR_INTEREST_SHARE_BPS,
            ErrorCode::InvalidInterestFeeBps
        );
        let mut next = *self;
        let fee_paid;
        if gross >= self.gross_unpaid {
            require_eq!(paid, 0, ErrorCode::BrokenInvariant);
            let issued = gross - self.gross_unpaid;
            if issued > 0 {
                require_eq!(self.pending_gross, 0, ErrorCode::BrokenInvariant);
                let protocol = split_with_fraction(
                    issued,
                    markup as u64,
                    BPS_DENOMINATOR as u64 + markup as u64,
                    &mut next.protocol_fraction_q64,
                )?;
                let operator = split_with_fraction(
                    issued - protocol,
                    operator_share as u64,
                    BPS_DENOMINATOR as u64,
                    &mut next.operator_fraction_q64,
                )?;
                next.protocol_unpaid = next
                    .protocol_unpaid
                    .checked_add(protocol)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                next.operator_unpaid = next
                    .operator_unpaid
                    .checked_add(operator)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            fee_paid = 0;
        } else {
            let removed = self.gross_unpaid - gross;
            require_gte!(removed, paid, ErrorCode::BrokenInvariant);
            let fees = self.unpaid_fees()?;
            let fee_removed = proportional(fees, removed, self.gross_unpaid)?;
            let protocol_removed = proportional(self.protocol_unpaid, fee_removed, fees)?;
            fee_paid = proportional(fee_removed, paid, removed)?;
            let protocol_paid = proportional(protocol_removed, fee_paid, fee_removed)?;
            next.protocol_unpaid -= protocol_removed;
            next.operator_unpaid = next
                .operator_unpaid
                .checked_sub(fee_removed - protocol_removed)
                .ok_or(ErrorCode::BrokenInvariant)?;
            next.pending_gross = next
                .pending_gross
                .checked_add(fee_paid)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            next.pending_protocol_gross = next
                .pending_protocol_gross
                .checked_add(protocol_paid)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        next.gross_unpaid = gross;
        require_gte!(gross, next.unpaid_fees()?, ErrorCode::BrokenInvariant);
        *self = next;
        Ok(fee_paid)
    }
    pub fn quote_cash(&self, credit: u64, total_gross: u64) -> Result<(u64, u64)> {
        require_gte!(total_gross, credit, ErrorCode::UnbackedFeeLiability);
        require_gte!(total_gross, self.pending_gross, ErrorCode::BrokenInvariant);
        let fee_cash = proportional(credit, self.pending_gross, total_gross)?;
        let mut fraction = self.protocol_cash_fraction_q64;
        let protocol = if self.pending_gross == 0 {
            0
        } else {
            split_with_fraction(fee_cash, self.pending_protocol_gross, self.pending_gross, &mut fraction)?
        };
        Ok((fee_cash, protocol))
    }
    pub fn collect(&mut self, fee_cash: u64) -> Result<u64> {
        require_gte!(self.pending_gross, fee_cash, ErrorCode::UnbackedFeeLiability);
        let mut next = *self;
        let protocol = if self.pending_gross == 0 {
            0
        } else {
            split_with_fraction(
                fee_cash,
                self.pending_protocol_gross,
                self.pending_gross,
                &mut next.protocol_cash_fraction_q64,
            )?
        };
        next.operator_claimable = next
            .operator_claimable
            .checked_add(fee_cash - protocol)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        next.pending_gross = 0;
        next.pending_protocol_gross = 0;
        *self = next;
        Ok(protocol)
    }
    pub fn take_operator_cash(&mut self) -> Result<u64> {
        let cash = self.operator_claimable;
        self.operator_claimable = 0;
        Ok(cash)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InterestRevenueCredit {
    pub lp: u64,
    pub fee: u64,
    pub protocol: u64,
    pub available_fee_custody: u64,
}
#[cfg(test)]
mod tests {
    include!("../tests/transitions/operator_revenue.rs");
}
