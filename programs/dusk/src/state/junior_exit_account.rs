use anchor_lang::prelude::*;

/// Notice uses scheduled slots. The exercise window counts only slots without
/// a verified conversion restriction; notice itself is not paused. Handlers
/// authenticate owner/escrow and derive blocked time from protocol records.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct JuniorExitTicket {
    pub generation: u64,
    pub receipts: u64,
    pub ready_at: u64,
    /// Exclusive end before adding verified blocked time after ready_at.
    pub expires_at: u64,
}

/// Burned receipt units in this account remain junior claims. They earn income
/// through the owner's yield accounts and cannot be transferred or spent twice.
#[account]
#[derive(InitSpace, Default)]
pub struct JuniorExitAccount {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub ticket: JuniorExitTicket,
    pub bump: u8,
}
