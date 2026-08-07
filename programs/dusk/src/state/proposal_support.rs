use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

use super::{accrue_fee_liability_with_remainder, YieldAccount};

/// A yield ledger for yLP burned into one support position. Its checkpoints
/// isolate each proposal lock while the user's ordinary YieldAccounts continue
/// to follow only the live balance in their Token-2022 ATA.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct VirtualYieldLedger {
    pub swap_fee_checkpoint_q64: u128,
    pub interest_checkpoint_q64: u128,
    pub accrued_swap_fee_amount: u64,
    pub accrued_interest_amount: u64,
    pub swap_fee_remainder_q64: u64,
    pub interest_remainder_q64: u64,
}

impl VirtualYieldLedger {
    pub fn initialize(&mut self, swap_fee_growth_index_q64: u128, interest_growth_index_q64: u128) {
        self.swap_fee_checkpoint_q64 = swap_fee_growth_index_q64;
        self.interest_checkpoint_q64 = interest_growth_index_q64;
    }

    pub fn accrue(
        &mut self,
        locked_amount: u64,
        swap_fee_growth_index_q64: u128,
        interest_growth_index_q64: u128,
    ) -> Result<()> {
        let (swap_fee_amount, swap_fee_remainder_q64) = accrue_fee_liability_with_remainder(
            locked_amount,
            swap_fee_growth_index_q64,
            self.swap_fee_checkpoint_q64,
            self.swap_fee_remainder_q64,
        )?;
        let (interest_amount, interest_remainder_q64) = accrue_fee_liability_with_remainder(
            locked_amount,
            interest_growth_index_q64,
            self.interest_checkpoint_q64,
            self.interest_remainder_q64,
        )?;
        self.accrued_swap_fee_amount = self
            .accrued_swap_fee_amount
            .checked_add(swap_fee_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.accrued_interest_amount = self
            .accrued_interest_amount
            .checked_add(interest_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.swap_fee_remainder_q64 = swap_fee_remainder_q64;
        self.interest_remainder_q64 = interest_remainder_q64;
        self.swap_fee_checkpoint_q64 = swap_fee_growth_index_q64;
        self.interest_checkpoint_q64 = interest_growth_index_q64;
        Ok(())
    }

    pub fn merge_into(self, yield_account: &mut YieldAccount) -> Result<()> {
        yield_account.credit_unallocated(
            self.accrued_swap_fee_amount,
            self.accrued_interest_amount,
            self.swap_fee_remainder_q64 as u128,
            self.interest_remainder_q64 as u128,
        )
    }
}

#[account]
#[derive(InitSpace)]
pub struct ProposalSupport {
    pub proposal: Pubkey,
    pub supporter: Pubkey,
    pub locked_amount: u64,
    pub base_yield: VirtualYieldLedger,
    pub quote_yield: VirtualYieldLedger,
    pub bump: u8,
}

impl ProposalSupport {
    pub fn initialize(
        &mut self,
        proposal: Pubkey,
        supporter: Pubkey,
        base_swap_fee_growth_index_q64: u128,
        base_interest_growth_index_q64: u128,
        quote_swap_fee_growth_index_q64: u128,
        quote_interest_growth_index_q64: u128,
        bump: u8,
    ) {
        self.proposal = proposal;
        self.supporter = supporter;
        self.locked_amount = 0;
        self.base_yield = VirtualYieldLedger::default();
        self.base_yield
            .initialize(base_swap_fee_growth_index_q64, base_interest_growth_index_q64);
        self.quote_yield = VirtualYieldLedger::default();
        self.quote_yield
            .initialize(quote_swap_fee_growth_index_q64, quote_interest_growth_index_q64);
        self.bump = bump;
    }

    pub fn assert_account(&self, proposal: Pubkey, supporter: Pubkey, bump: u8) -> Result<()> {
        require_keys_eq!(self.proposal, proposal, ErrorCode::InvalidProposalSupport);
        require_keys_eq!(self.supporter, supporter, ErrorCode::InvalidProposalSupport);
        require_eq!(self.bump, bump, ErrorCode::InvalidProposalSupport);
        Ok(())
    }

    pub fn accrue_virtual_yield(
        &mut self,
        base_swap_fee_growth_index_q64: u128,
        base_interest_growth_index_q64: u128,
        quote_swap_fee_growth_index_q64: u128,
        quote_interest_growth_index_q64: u128,
    ) -> Result<()> {
        self.base_yield.accrue(
            self.locked_amount,
            base_swap_fee_growth_index_q64,
            base_interest_growth_index_q64,
        )?;
        self.quote_yield.accrue(
            self.locked_amount,
            quote_swap_fee_growth_index_q64,
            quote_interest_growth_index_q64,
        )
    }
}
