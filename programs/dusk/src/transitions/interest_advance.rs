//! Cash-funded public interest. Borrower debt remains unchanged when LP yield
//! is funded; repayment replenishes the advance instead of paying yield twice.

use crate::{
    errors::ErrorCode,
    math::mul_div_u128,
    state::{Market, MarketAsset, PublicInterestAdvance},
};
use anchor_lang::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InterestAdvanceClearance {
    pub repaid: u64,
    pub canceled: u64,
    pub protocol_returned: u64,
}

impl PublicInterestAdvance {
    pub(crate) fn require_settled(self) -> Result<()> {
        require_eq!(self.repayment_credit_pending, 0, ErrorCode::BrokenInvariant);
        require_eq!(self.protocol_release_pending, 0, ErrorCode::BrokenInvariant);
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
        self.protocol_escrow = self
            .protocol_escrow
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
        require_gte!(self.outstanding_gross, self.protocol_escrow, ErrorCode::BrokenInvariant);
        let removed = unpaid_before.saturating_sub(unpaid_after);
        if self.outstanding_gross == 0 || removed == 0 {
            return Ok(InterestAdvanceClearance::default());
        }
        let proportional = |amount: u64, part: u128, total: u128| -> Result<u64> {
            u64::try_from(mul_div_u128(u128::from(amount), part, total)?)
                .map_err(|_| ErrorCode::MarketMathOverflow.into())
        };
        let advanced_removed = proportional(self.outstanding_gross, removed, unpaid_before)?;
        let paid = proportional(advanced_removed, u128::from(interest_paid).min(removed), removed)?;
        let protocol_removed = proportional(
            self.protocol_escrow,
            u128::from(advanced_removed),
            u128::from(self.outstanding_gross),
        )?;
        let protocol_paid = if advanced_removed == 0 {
            0
        } else {
            proportional(protocol_removed, u128::from(paid), u128::from(advanced_removed))?
        };
        self.outstanding_gross -= advanced_removed;
        self.protocol_escrow -= protocol_removed;
        self.repayment_credit_pending = self
            .repayment_credit_pending
            .checked_add(paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.protocol_release_pending = self
            .protocol_release_pending
            .checked_add(protocol_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(InterestAdvanceClearance {
            repaid: paid,
            canceled: advanced_removed - paid,
            protocol_returned: protocol_removed - protocol_paid,
        })
    }

    /// Return the new gross interest and frozen protocol gross which must be
    /// moved to interest custody. The balance recycled to reserves is excluded.
    pub(crate) fn take_payment(&mut self, interest_paid: u64) -> Result<(u64, u64)> {
        require_gte!(interest_paid, self.repayment_credit_pending, ErrorCode::BrokenInvariant);
        require_gte!(
            self.repayment_credit_pending,
            self.protocol_release_pending,
            ErrorCode::BrokenInvariant
        );
        let fresh = interest_paid - self.repayment_credit_pending;
        let frozen_protocol = self.protocol_release_pending;
        self.repayment_credit_pending = 0;
        self.protocol_release_pending = 0;
        Ok((fresh, frozen_protocol))
    }
}

impl Market {
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
        let gross = u64::try_from(
            unfunded
                .min(u128::from(max_gross))
                .min(u128::from(side.reserves.cash_reserve)),
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let (protocol, _) = crate::transitions::revenue::split_revenue(gross, protocol_bps)?;
        side.fees.public_interest_advance.fund(unpaid, gross, protocol)?;
        side.reserves.cash_reserve -= gross;
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_sub(gross)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        self.sync_income_receivable(asset, false, gross)?;
        Ok((gross, gross - protocol))
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
            .checked_add(cleared.protocol_returned)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        side.reserves.cash_reserve = side
            .reserves
            .cash_reserve
            .checked_add(cash_credit)
            .and_then(|v| v.checked_add(restored))
            .ok_or(ErrorCode::ReserveOverflow)?;
        let live = u128::from(side.reserves.live_reserve) + u128::from(restored);
        side.reserves.live_reserve = u64::try_from(
            live.checked_sub(u128::from(live_debit))
                .ok_or(ErrorCode::ReserveUnderflow)?,
        )
        .map_err(|_| ErrorCode::ReserveOverflow)?;
        let unpaid_removed = unpaid_before.saturating_sub(unpaid_after);
        let funded_debit = (live_debit as u128)
            .checked_add(cleared.canceled as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let unfunded_debit = unpaid_removed
            .checked_add(cleared.protocol_returned as u128)
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
    use super::*;

    #[test]
    fn share_floor_growth_does_not_release_or_cancel_an_interest_advance() {
        let mut advance = PublicInterestAdvance::default();
        advance.fund(100, 80, 16).unwrap();
        let before = advance;
        assert!(advance.clear(100, 101, 1).is_err());
        assert_eq!(advance, before);
        let cleared = advance.clear(100, 101, 0).unwrap();
        assert_eq!((cleared.repaid, cleared.canceled, cleared.protocol_returned), (0, 0, 0));
        assert_eq!(advance, before);
        assert!(advance.clear(101, 103, 0).is_err());
        assert_eq!(advance, before);
        let cleared = advance.clear(101, 0, 101).unwrap();
        assert_eq!((cleared.repaid, cleared.canceled), (80, 0));
        assert_eq!(advance.take_payment(101).unwrap(), (21, 16));
        assert_eq!(advance, PublicInterestAdvance::default());
        assert!(advance.clear(0, 1, 1).is_err());
        assert_eq!(advance, PublicInterestAdvance::default());
    }

    #[test]
    fn frozen_split_survives_later_rate_changes_and_partial_repayment() {
        let mut a = PublicInterestAdvance::default();
        a.fund(100, 100, 20).unwrap();
        // A later 30% protocol rate changes only the next advance.
        a.fund(200, 100, 30).unwrap();
        let c = a.clear(200, 100, 100).unwrap();
        assert_eq!((c.repaid, c.canceled, c.protocol_returned), (100, 0, 0));
        assert_eq!(a.take_payment(100).unwrap(), (0, 25));
        let c = a.clear(100, 0, 100).unwrap();
        assert_eq!(c.repaid, 100);
        assert_eq!(a.take_payment(100).unwrap(), (0, 25));
        assert_eq!(a, PublicInterestAdvance::default());
    }

    #[test]
    fn canceled_advance_returns_held_protocol_but_does_not_invent_lp_cash() {
        let mut a = PublicInterestAdvance::default();
        a.fund(100, 80, 16).unwrap();
        let c = a.clear(100, 0, 25).unwrap();
        assert_eq!((c.repaid, c.canceled, c.protocol_returned), (20, 60, 12));
        assert_eq!(a.take_payment(25).unwrap(), (5, 4));
        assert_eq!(a, PublicInterestAdvance::default());
    }

    #[test]
    fn partial_clearances_conserve_escrow_and_advance_atoms() {
        for gross in 1..100_u64 {
            for protocol in 0..=gross {
                let mut a = PublicInterestAdvance::default();
                a.fund(101, gross, protocol).unwrap();
                let mut advanced = 0;
                let mut released = 0;
                for before in (1..=101).rev() {
                    let paid = if before % 3 == 0 { 0 } else { 1 };
                    let c = a.clear(before, before - 1, paid).unwrap();
                    advanced += c.repaid + c.canceled;
                    released += c.protocol_returned;
                    let (fresh, frozen) = a.take_payment(paid).unwrap();
                    assert!(fresh <= paid);
                    released += frozen;
                }
                assert_eq!(advanced, gross);
                assert_eq!(released, protocol);
                assert_eq!(a, PublicInterestAdvance::default());
            }
        }
    }
}
