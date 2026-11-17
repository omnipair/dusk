//! Cash-funded public interest. Borrower debt remains unchanged when LP yield
//! is funded; repayment replenishes the advance instead of paying yield twice.

use crate::{
    errors::ErrorCode,
    math::{mul_div_ceil_u128, mul_div_u128},
    state::{Market, MarketAsset, PublicInterestAdvance},
    transitions::LeverageCollateralFee,
};
use anchor_lang::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InterestAdvanceClearance {
    pub repaid: u64,
    pub canceled: u64,
    pub revenue_returned: u64,
}

impl PublicInterestAdvance {
    pub(crate) fn require_settled(self) -> Result<()> {
        require_eq!(self.repayment_credit_pending, 0, ErrorCode::BrokenInvariant);
        require_eq!(self.revenue_release_pending, 0, ErrorCode::BrokenInvariant);
        Ok(())
    }

    pub(crate) fn fund(&mut self, unpaid: u128, gross: u64, protocol: u64) -> Result<()> {
        self.require_settled()?;
        require_gte!(gross, protocol, ErrorCode::FeeMathOverflow);
        let outstanding = self
            .outstanding_gross
            .checked_add(gross)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(u128::from(outstanding) <= unpaid, ErrorCode::BrokenInvariant);
        self.outstanding_gross = outstanding;
        self.revenue_escrow = self
            .revenue_escrow
            .checked_add(protocol)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    /// Repayment/default risk is pooled across outstanding public interest.
    /// Every clearance removes its proportional advance and frozen fee. The
    /// final clearance drains all rounding remainders. Rates are never read here.
    pub(crate) fn clear(
        &mut self,
        unpaid_before: u128,
        unpaid_after: u128,
        interest_paid: u64,
    ) -> Result<InterestAdvanceClearance> {
        // A partial share burn can remove one more position-principal atom
        // than aggregate debt. That creates one unpaid-interest atom without
        // clearing any existing advance; no advance or escrow is released.
        if unpaid_after > unpaid_before {
            require_eq!(unpaid_after - unpaid_before, 1, ErrorCode::BrokenInvariant);
            require_eq!(interest_paid, 0, ErrorCode::BrokenInvariant);
        }
        require!(
            u128::from(self.outstanding_gross) <= unpaid_before,
            ErrorCode::BrokenInvariant
        );
        require_gte!(self.outstanding_gross, self.revenue_escrow, ErrorCode::BrokenInvariant);
        let removed = unpaid_before.saturating_sub(unpaid_after);
        if self.outstanding_gross == 0 || removed == 0 {
            return Ok(InterestAdvanceClearance::default());
        }
        let proportional = |amount: u64, part: u128, total: u128| -> Result<u64> {
            u64::try_from(mul_div_u128(u128::from(amount), part, total)?)
                .map_err(|_| ErrorCode::MarketMathOverflow.into())
        };
        // Clear the LP-funded and frozen-fee portions separately. Remaining
        // LP advances round down, like the remaining LP receivable; remaining
        // escrow rounds up, like unpaid fees. Rounding gross advances first
        // can recycle one fee atom to reserves and leave collection unfunded.
        let lp_removed = u64::try_from(mul_div_ceil_u128(
            (self.outstanding_gross - self.revenue_escrow) as u128,
            removed,
            unpaid_before,
        )?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let protocol_removed = proportional(self.revenue_escrow, removed, unpaid_before)?;
        let advanced_removed = lp_removed + protocol_removed;
        let paid_part = u128::from(interest_paid).min(removed);
        let protocol_paid = proportional(protocol_removed, paid_part, removed)?;
        // Fees round paid amounts down; round recycled LP cash up so the
        // remainder cannot pay more than the unfunded LP claim being cleared.
        let lp_paid = u64::try_from(mul_div_ceil_u128(lp_removed as u128, paid_part, removed)?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let paid = lp_paid + protocol_paid;
        self.outstanding_gross -= advanced_removed;
        self.revenue_escrow -= protocol_removed;
        self.repayment_credit_pending = self
            .repayment_credit_pending
            .checked_add(paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.revenue_release_pending = self
            .revenue_release_pending
            .checked_add(protocol_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(InterestAdvanceClearance {
            repaid: paid,
            canceled: advanced_removed - paid,
            revenue_returned: protocol_removed - protocol_paid,
        })
    }

    /// Return the new gross interest and frozen protocol gross which must be
    /// moved to interest custody. The balance recycled to reserves is excluded.
    pub(crate) fn take_payment(&mut self, interest_paid: u64) -> Result<(u64, u64)> {
        require_gte!(interest_paid, self.repayment_credit_pending, ErrorCode::BrokenInvariant);
        require_gte!(
            self.repayment_credit_pending,
            self.revenue_release_pending,
            ErrorCode::BrokenInvariant
        );
        let fresh = interest_paid - self.repayment_credit_pending;
        let frozen_protocol = self.revenue_release_pending;
        self.repayment_credit_pending = 0;
        self.revenue_release_pending = 0;
        Ok((fresh, frozen_protocol))
    }
}

/// Smallest wallet debit that credits `credit` under `fee`.
fn gross_for_credit(fee: LeverageCollateralFee, credit: u64) -> Result<u64> {
    credit
        .checked_add(fee.inverse_fee(credit)?)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

/// Reserve credit a direct repayment may use within the borrower's gross
/// limit `max_gross`, under the debt mint's effective transfer fee. The
/// borrower also pays the fee on the interest the reserve then moves into
/// income custody, which is at most the repayment. When the limit leaves room
/// for that fee the repayment is unchanged and the fee is charged on top;
/// only when the limit binds is the repayment reduced, by at most the fee on
/// the whole repayment.
pub(crate) fn direct_repayment_credit(
    fee: LeverageCollateralFee,
    max_gross: u64,
    repayment_for_max: impl Fn(u64) -> Result<u64>,
) -> Result<u64> {
    let max_credit = fee.unwind_credit(max_gross)?;
    let credit = repayment_for_max(max_credit)?;
    let fee_bound = fee.inverse_fee(credit)?;
    let with_fee = credit.checked_add(fee_bound).ok_or(ErrorCode::MarketMathOverflow)?;
    if fee_bound == 0 || gross_for_credit(fee, with_fee)? <= max_gross {
        return Ok(credit);
    }
    repayment_for_max(max_credit.saturating_sub(fee_bound))
}

/// Wallet debit and reserve credit for a direct repayment whose transition
/// already ran: the repayment plus `interest_transfer_fee`, the transfer fee
/// on the interest the reserve moves into income custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DirectRepaymentTransfer {
    pub gross: u64,
    pub reserve_credit: u64,
    pub interest_transfer_fee: u64,
}

impl Market {
    pub(crate) fn direct_repayment_transfer(
        &self,
        asset: MarketAsset,
        fee: LeverageCollateralFee,
        repay_credit: u64,
        interest_paid: u64,
        max_gross: u64,
    ) -> Result<DirectRepaymentTransfer> {
        // The gross `take_payment(interest_paid)` will move to interest
        // custody, read before the borrower's transfer.
        let advance = self.side(asset).fees.public_interest_advance;
        require_gte!(
            interest_paid,
            advance.repayment_credit_pending,
            ErrorCode::BrokenInvariant
        );
        let interest_transfer = (interest_paid - advance.repayment_credit_pending)
            .checked_add(advance.revenue_release_pending)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let interest_transfer_fee = fee.inverse_fee(interest_transfer)?;
        let reserve_credit = repay_credit
            .checked_add(interest_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let gross = gross_for_credit(fee, reserve_credit)?;
        require_gte!(max_gross, gross, ErrorCode::BrokenInvariant);
        Ok(DirectRepaymentTransfer {
            gross,
            reserve_credit,
            interest_transfer_fee,
        })
    }

    /// `fund_public_interest` whose LP transfer is grossed up by the asset's
    /// transfer fee, which reserve cash also pays
    /// (`pay_settled_interest_transfer_fee`), so custody receives the full
    /// advance. `available` is the reserve cash the advance may use; the fee
    /// is kept inside it by capping the advance at what `available` would
    /// deliver through one transfer, since the smallest grossed-up transfer
    /// of anything below that is at most `available`. Returns the gross
    /// advance, the LP transfer, and the fee paid on top of it.
    pub fn fund_public_interest_paying_transfer_fee(
        &mut self,
        asset: MarketAsset,
        max_gross: u64,
        available: u64,
        protocol_bps: u16,
        fee: LeverageCollateralFee,
        slot: u64,
    ) -> Result<(u64, u64, u64)> {
        let (gross, lp_gross) =
            self.fund_public_interest(asset, max_gross.min(fee.unwind_credit(available)?), protocol_bps)?;
        let transfer_fee = self.pay_settled_interest_transfer_fee(asset, fee.inverse_fee(lp_gross)?, slot)?;
        Ok((gross, lp_gross, transfer_fee))
    }

    /// Reserve cash replaces a public interest receivable in LP yield custody.
    /// Borrower debt stays outstanding; frozen protocol revenue remains in the
    /// reserve vault and is payable only when that interest is actually repaid.
    /// This is the native accounting leg, not a standalone harvest: the caller
    /// must move measured LP custody credit and complete hLP net-yield settlement
    /// before publishing holder claims. `fund_interest` executes this full sequence.
    pub fn fund_public_interest(
        &mut self,
        asset: MarketAsset,
        max_gross: u64,
        protocol_bps: u16,
    ) -> Result<(u64, u64)> {
        require_gte!(
            crate::constants::BPS_DENOMINATOR,
            protocol_bps,
            ErrorCode::InvalidInterestFeeBps
        );
        self.sync_income_receivable(asset, false, 0)?;
        let unpaid = self.public_unpaid_interest(asset)?;
        let unfunded = unpaid
            .checked_sub(u128::from(
                self.side(asset).fees.public_interest_advance.outstanding_gross,
            ))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let side = self.side_mut(asset);
        let mut gross = unfunded
            .min(u128::from(max_gross))
            .min(u128::from(side.reserves.cash_reserve));
        if gross < unfunded && self.side(asset).fees.junior.initialized {
            // Never leave the income book a dust remainder. Accrual issues
            // units at the book's unit value, so a remainder priced below
            // 2^-16 per unit multiplies the unit count with each round and
            // can exhaust the unit range. Fund the whole coupon or leave at
            // least that floor unfunded.
            let floor = self.income.get()?.books[asset.code() as usize].units_q64 >> 80;
            gross = gross.min(unfunded.saturating_sub(floor));
        }
        let gross = u64::try_from(gross).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let fee = if self.side(asset).fees.junior.initialized {
            let i = asset.code() as usize;
            let fee_unpaid = self.income.get()?.revenue[i]
                .unpaid_fees()?
                .checked_sub(self.side(asset).fees.public_interest_advance.revenue_escrow)
                .ok_or(ErrorCode::BrokenInvariant)?;
            if unfunded == 0 {
                0
            } else {
                u64::try_from(mul_div_u128(fee_unpaid as u128, gross as u128, unfunded)?)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?
            }
        } else {
            crate::transitions::revenue::split_revenue(gross, protocol_bps)?.0
        };
        let side = self.side_mut(asset);
        side.fees.public_interest_advance.fund(unpaid, gross, fee)?;
        side.reserves.cash_reserve -= gross;
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_sub(u128::from(gross))
            .ok_or(ErrorCode::ReserveUnderflow)?;
        self.sync_income_receivable(asset, false, gross - fee)?;
        Ok((gross, gross - fee))
    }

    pub(crate) fn settle_public_debt_reserves(
        &mut self,
        asset: MarketAsset,
        unpaid_before: u128,
        interest_paid: u64,
        live_debit: u64,
        cash_credit: u64,
    ) -> Result<InterestAdvanceClearance> {
        let unpaid_after = self.public_unpaid_interest(asset)?;
        let side = self.side_mut(asset);
        let cleared = side
            .fees
            .public_interest_advance
            .clear(unpaid_before, unpaid_after, interest_paid)?;
        let restored = cleared
            .repaid
            .checked_add(cleared.revenue_returned)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        side.reserves.cash_reserve = side
            .reserves
            .cash_reserve
            .checked_add(cash_credit)
            .and_then(|v| v.checked_add(restored))
            .ok_or(ErrorCode::ReserveOverflow)?;
        let live = side.reserves.live_reserve + u128::from(restored);
        side.reserves.live_reserve = live
            .checked_sub(u128::from(live_debit))
            .ok_or(ErrorCode::ReserveUnderflow)?;
        let unpaid_removed = unpaid_before.saturating_sub(unpaid_after);
        let funded_debit = (live_debit as u128)
            .checked_add(cleared.canceled as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let unfunded_debit = unpaid_removed
            .checked_add(cleared.revenue_returned as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let funded_loss =
            u64::try_from(funded_debit.saturating_sub(unfunded_debit)).map_err(|_| ErrorCode::MarketMathOverflow)?;
        self.sync_income_receivable(asset, false, interest_paid - cleared.repaid)?;
        self.absorb_funded_credit_loss(asset, funded_loss)?;
        Ok(cleared)
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/transitions/interest_advance.rs");
}
