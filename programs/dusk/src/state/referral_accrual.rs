use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

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
