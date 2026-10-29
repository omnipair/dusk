use anchor_lang::prelude::*;

use super::VirtualYieldLedger;
use crate::{constants::GOVERNANCE_BALANCE_SEED_PREFIX, errors::ErrorCode};

/// Owner-held virtual yLP. Both idle and committed shares accrue yield here.
#[account]
#[derive(InitSpace)]
pub struct GovernanceBalance {
    pub market: Pubkey,
    pub owner: Pubkey,
    /// The default key revokes delegation. The owner always retains control.
    pub delegate: Pubkey,
    pub total_amount: u64,
    pub committed_amount: u64,
    pub base_yield: VirtualYieldLedger,
    pub quote_yield: VirtualYieldLedger,
    pub bump: u8,
}

impl GovernanceBalance {
    pub fn assert_account(&self, market: Pubkey, key: Pubkey) -> Result<()> {
        require_keys_eq!(self.market, market, ErrorCode::InvalidProposalSupport);
        let expected = Pubkey::create_program_address(
            &[
                GOVERNANCE_BALANCE_SEED_PREFIX,
                market.as_ref(),
                self.owner.as_ref(),
                &[self.bump],
            ],
            &crate::ID,
        )
        .map_err(|_| error!(ErrorCode::InvalidProposalSupport))?;
        require_keys_eq!(key, expected, ErrorCode::InvalidProposalSupport);
        require_gte!(
            self.total_amount,
            self.committed_amount,
            ErrorCode::InvalidProposalSupport
        );
        Ok(())
    }

    pub fn authorize(&self, authority: Pubkey) -> Result<()> {
        require!(
            authority == self.owner || (self.delegate != Pubkey::default() && authority == self.delegate),
            ErrorCode::InvalidSigner
        );
        Ok(())
    }

    pub fn available(&self) -> Result<u64> {
        self.total_amount
            .checked_sub(self.committed_amount)
            .ok_or_else(|| ErrorCode::InvalidProposalSupport.into())
    }

    pub fn commit(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        require_gte!(self.available()?, amount, ErrorCode::InsufficientBalance);
        self.committed_amount = self
            .committed_amount
            .checked_add(amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }
}
