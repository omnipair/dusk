use anchor_lang::prelude::*;

/// One owner's reusable balance allocated to one proposal. Yield stays on the
/// balance, so allocation/release cannot duplicate or transfer yield claims.
#[account]
#[derive(InitSpace)]
pub struct GovernanceSupport {
    pub proposal: Pubkey,
    pub governance_balance: Pubkey,
    pub amount: u64,
    pub rent_refund: Pubkey,
    pub bump: u8,
}
