use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, MAX_REFERRAL_ORIGINATION_FEE_BPS},
    errors::ErrorCode,
    shared::math::ceil_div,
};

#[account]
#[derive(Debug, InitSpace)]
pub struct ReferralProfile {
    pub authority: Pubkey,
    pub recipient: Pubkey,
    pub bump: u8,
}

impl ReferralProfile {
    pub fn initialize(&mut self, authority: Pubkey, recipient: Pubkey, bump: u8) -> Result<()> {
        require_keys_neq!(authority, Pubkey::default(), ErrorCode::InvalidReferralProfile);
        require_keys_neq!(recipient, Pubkey::default(), ErrorCode::InvalidRecipient);
        self.authority = authority;
        self.recipient = recipient;
        self.bump = bump;
        Ok(())
    }

    pub fn set_recipient(&mut self, authority: Pubkey, recipient: Pubkey) -> Result<()> {
        require_keys_eq!(self.authority, authority, ErrorCode::InvalidReferralProfile);
        require_keys_neq!(recipient, Pubkey::default(), ErrorCode::InvalidRecipient);
        self.recipient = recipient;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferralFeeQuote {
    pub requested_principal: u64,
    pub configured_fee_bps: u16,
    pub fee_debit: u64,
    pub gross_debt: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferralFeeReceipt {
    pub requested_principal: u64,
    pub configured_fee_bps: u16,
    pub fee_debit: u64,
    pub vault_credit: u64,
    pub gross_debt: u64,
}

impl ReferralFeeReceipt {
    pub fn new(quote: ReferralFeeQuote, vault_credit: u64) -> Result<Self> {
        require_gte!(quote.fee_debit, vault_credit, ErrorCode::MarketMathOverflow);
        Ok(Self {
            requested_principal: quote.requested_principal,
            configured_fee_bps: quote.configured_fee_bps,
            fee_debit: quote.fee_debit,
            vault_credit,
            gross_debt: quote.gross_debt,
        })
    }
}

impl ReferralFeeQuote {
    pub fn new(
        requested_principal: u64,
        configured_fee_bps: u16,
        max_acceptable_fee_bps: u16,
        referred: bool,
    ) -> Result<Self> {
        require_gte!(
            MAX_REFERRAL_ORIGINATION_FEE_BPS,
            configured_fee_bps,
            ErrorCode::InvalidReferralFeeBps
        );
        if !referred {
            return Ok(Self {
                requested_principal,
                gross_debt: requested_principal,
                ..Self::default()
            });
        }
        require_gte!(
            max_acceptable_fee_bps,
            configured_fee_bps,
            ErrorCode::ReferralFeeSlippageExceeded
        );
        let fee_debit = ceil_div(
            (requested_principal as u128)
                .checked_mul(configured_fee_bps as u128)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            BPS_DENOMINATOR as u128,
        )
        .ok_or(ErrorCode::FeeMathOverflow)?;
        let fee_debit = u64::try_from(fee_debit).map_err(|_| ErrorCode::FeeMathOverflow)?;
        let gross_debt = requested_principal
            .checked_add(fee_debit)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        Ok(Self {
            requested_principal,
            configured_fee_bps,
            fee_debit,
            gross_debt,
        })
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferralAction {
    Borrow,
    OpenLeverage,
    IncreaseLeverage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referral_fee_quote_rounds_up_and_adds_to_debt() {
        let quote = ReferralFeeQuote::new(10_001, 10, 10, true).unwrap();
        assert_eq!(quote.fee_debit, 11);
        assert_eq!(quote.gross_debt, 10_012);
    }

    #[test]
    fn referral_fee_quote_supports_zero_and_hard_cap() {
        assert_eq!(ReferralFeeQuote::new(1, 0, 0, true).unwrap().fee_debit, 0);
        assert_eq!(ReferralFeeQuote::new(1, 25, 25, true).unwrap().fee_debit, 1);
        assert_eq!(
            ReferralFeeQuote::new(1, 26, 26, true).unwrap_err(),
            error!(ErrorCode::InvalidReferralFeeBps)
        );
    }

    #[test]
    fn referral_fee_quote_enforces_caller_maximum() {
        assert_eq!(
            ReferralFeeQuote::new(1_000, 10, 9, true).unwrap_err(),
            error!(ErrorCode::ReferralFeeSlippageExceeded)
        );
        let quote = ReferralFeeQuote::new(1_000, 10, 0, false).unwrap();
        assert_eq!(quote.fee_debit, 0);
        assert_eq!(quote.gross_debt, 1_000);
    }

    #[test]
    fn referral_fee_quote_rejects_gross_debt_overflow() {
        assert_eq!(
            ReferralFeeQuote::new(u64::MAX, 1, 1, true).unwrap_err(),
            error!(ErrorCode::DebtMathOverflow)
        );
    }

    #[test]
    fn referral_fee_receipt_preserves_debit_and_actual_credit() {
        let quote = ReferralFeeQuote::new(10_000, 10, 10, true).unwrap();
        let receipt = ReferralFeeReceipt::new(quote, 9).unwrap();
        assert_eq!(receipt.requested_principal, 10_000);
        assert_eq!(receipt.configured_fee_bps, 10);
        assert_eq!(receipt.fee_debit, 10);
        assert_eq!(receipt.vault_credit, 9);
        assert_eq!(receipt.gross_debt, 10_010);
    }
}
