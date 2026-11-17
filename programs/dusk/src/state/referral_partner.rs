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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferralInterestQuote {
    pub interest_paid: u64,
    pub protocol_interest_revenue: u64,
    pub interest_share_bps: u16,
    pub referral_amount: u64,
}

impl ReferralInterestQuote {
    /// The referral share comes only from the protocol share of this
    /// payment.
    pub(crate) fn with_protocol_revenue(
        interest_paid: u64,
        protocol_interest_revenue: u64,
        interest_share_bps: Option<u16>,
    ) -> Result<Self> {
        require_gte!(interest_paid, protocol_interest_revenue, ErrorCode::FeeMathOverflow);
        let Some(interest_share_bps) = interest_share_bps else {
            return Ok(Self {
                interest_paid,
                protocol_interest_revenue,
                ..Self::default()
            });
        };
        validate_interest_share_bps(interest_share_bps)?;
        let referral_amount = u64::try_from(
            u128::from(protocol_interest_revenue) * u128::from(interest_share_bps) / u128::from(BPS_DENOMINATOR),
        )
        .map_err(|_| ErrorCode::FeeMathOverflow)?;
        Ok(Self {
            interest_paid,
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

#[cfg(test)]
mod tests {
    include!("../tests/state/referral_partner.rs");
}
