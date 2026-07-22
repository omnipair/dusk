use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, MAX_REFERRAL_INTEREST_SHARE_BPS},
    errors::ErrorCode,
};

/// A permissioned, protocol-wide referral registry entry.
#[account]
#[derive(Debug, InitSpace)]
pub struct ReferralPartner {
    pub authority: Pubkey,
    pub recipient: Pubkey,
    pub interest_share_bps: u16,
    pub active: bool,
    pub bump: u8,
}

impl ReferralPartner {
    pub fn initialize(&mut self, authority: Pubkey, interest_share_bps: u16, active: bool, bump: u8) -> Result<()> {
        require_keys_neq!(authority, Pubkey::default(), ErrorCode::InvalidReferralPartner);
        validate_interest_share_bps(interest_share_bps)?;
        self.authority = authority;
        self.recipient = authority;
        self.interest_share_bps = interest_share_bps;
        self.active = active;
        self.bump = bump;
        Ok(())
    }

    pub fn configure(&mut self, authority: Pubkey, interest_share_bps: u16, active: bool) -> Result<()> {
        require_keys_eq!(self.authority, authority, ErrorCode::InvalidReferralPartner);
        validate_interest_share_bps(interest_share_bps)?;
        self.interest_share_bps = interest_share_bps;
        self.active = active;
        Ok(())
    }

    pub fn set_recipient(&mut self, authority: Pubkey, recipient: Pubkey) -> Result<()> {
        require_keys_eq!(self.authority, authority, ErrorCode::InvalidReferralPartner);
        require_keys_neq!(recipient, Pubkey::default(), ErrorCode::InvalidRecipient);
        self.recipient = recipient;
        Ok(())
    }

    pub fn binding_interest_share_bps(&self, runtime_cap_bps: u16) -> Result<u16> {
        validate_interest_share_bps(runtime_cap_bps)?;
        require!(self.active, ErrorCode::ReferralPartnerNotActive);
        Ok(self.interest_share_bps.min(runtime_cap_bps))
    }
}

/// Claimable referral revenue for one partner, market, and debt asset.
#[account]
#[derive(Debug, InitSpace)]
pub struct ReferralAccrual {
    pub referral_partner: Pubkey,
    pub market: Pubkey,
    pub asset_mint: Pubkey,
    pub amount: u64,
    pub bump: u8,
}

impl ReferralAccrual {
    pub fn initialize(&mut self, referral_partner: Pubkey, market: Pubkey, asset_mint: Pubkey, bump: u8) -> Result<()> {
        require_keys_neq!(referral_partner, Pubkey::default(), ErrorCode::InvalidReferralAccrual);
        require_keys_neq!(market, Pubkey::default(), ErrorCode::InvalidReferralAccrual);
        require_keys_neq!(asset_mint, Pubkey::default(), ErrorCode::InvalidReferralAccrual);
        self.referral_partner = referral_partner;
        self.market = market;
        self.asset_mint = asset_mint;
        self.amount = 0;
        self.bump = bump;
        Ok(())
    }

    pub fn accrue(&mut self, amount: u64) -> Result<()> {
        self.amount = self.amount.checked_add(amount).ok_or(ErrorCode::FeeMathOverflow)?;
        Ok(())
    }

    pub fn claim(&mut self, amount: u64) -> Result<()> {
        self.amount = self.amount.checked_sub(amount).ok_or(ErrorCode::FeeMathOverflow)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferralInterestQuote {
    pub interest_paid: u64,
    pub interest_vault_credit: u64,
    pub protocol_interest_revenue: u64,
    pub interest_share_bps: u16,
    pub referral_amount: u64,
}

impl ReferralInterestQuote {
    pub fn new(
        interest_paid: u64,
        interest_vault_credit: u64,
        protocol_interest_bps: u16,
        interest_share_bps: Option<u16>,
    ) -> Result<Self> {
        require_gte!(BPS_DENOMINATOR, protocol_interest_bps, ErrorCode::InvalidInterestFeeBps);

        require_gte!(interest_paid, interest_vault_credit, ErrorCode::FeeMathOverflow);
        let protocol_interest_revenue = proportional_bps(interest_vault_credit, protocol_interest_bps)?;
        let Some(interest_share_bps) = interest_share_bps else {
            return Ok(Self {
                interest_paid,
                interest_vault_credit,
                protocol_interest_revenue,
                ..Self::default()
            });
        };
        validate_interest_share_bps(interest_share_bps)?;
        let referral_amount = proportional_bps(protocol_interest_revenue, interest_share_bps)?;
        Ok(Self {
            interest_paid,
            interest_vault_credit,
            protocol_interest_revenue,
            interest_share_bps,
            referral_amount,
        })
    }
}

fn validate_interest_share_bps(bps: u16) -> Result<()> {
    require_gte!(
        MAX_REFERRAL_INTEREST_SHARE_BPS,
        bps,
        ErrorCode::InvalidReferralInterestShareBps
    );
    Ok(())
}

fn proportional_bps(amount: u64, bps: u16) -> Result<u64> {
    let value = (amount as u128)
        .checked_mul(bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::FeeMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::FeeMathOverflow.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partner(interest_share_bps: u16, active: bool) -> ReferralPartner {
        ReferralPartner {
            authority: Pubkey::new_unique(),
            recipient: Pubkey::new_unique(),
            interest_share_bps,
            active,
            bump: 1,
        }
    }

    #[test]
    fn referral_share_is_taken_only_from_protocol_interest_revenue() {
        let quote = ReferralInterestQuote::new(100_000, 99_000, 2_000, Some(2_500)).unwrap();
        assert_eq!(quote.protocol_interest_revenue, 19_800);
        assert_eq!(quote.referral_amount, 4_950);
    }

    #[test]
    fn runtime_cap_is_snapshotted_when_referral_is_bound() {
        let partner = partner(7_500, true);
        let bound_share_bps = partner.binding_interest_share_bps(4_000).unwrap();
        let quote = ReferralInterestQuote::new(100_000, 100_000, 2_000, Some(bound_share_bps)).unwrap();
        assert_eq!(quote.interest_share_bps, 4_000);
        assert_eq!(quote.referral_amount, 8_000);
    }

    #[test]
    fn inactive_partner_cannot_be_bound() {
        let partner = partner(5_000, false);
        assert_eq!(
            partner.binding_interest_share_bps(10_000).unwrap_err(),
            error!(ErrorCode::ReferralPartnerNotActive)
        );
    }

    #[test]
    fn bound_share_survives_partner_deactivation_and_rate_changes() {
        let mut partner = partner(5_000, true);
        let bound_share_bps = partner.binding_interest_share_bps(10_000).unwrap();
        partner.configure(partner.authority, 1_000, false).unwrap();

        let quote = ReferralInterestQuote::new(100_000, 100_000, 2_000, Some(bound_share_bps)).unwrap();
        assert_eq!(quote.interest_share_bps, 5_000);
        assert_eq!(quote.referral_amount, 10_000);
    }

    #[test]
    fn tiny_payments_round_down() {
        let quote = ReferralInterestQuote::new(1, 1, 2_000, Some(5_000)).unwrap();
        assert_eq!(quote.protocol_interest_revenue, 0);
        assert_eq!(quote.referral_amount, 0);
    }
}
