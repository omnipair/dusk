use anchor_lang::prelude::*;

pub const RECOVERY_VOTE_SEED: &[u8] = b"recovery_vote";

#[account]
#[derive(InitSpace)]
pub struct RecoveryVote {
    pub ballot: Pubkey,
    pub round: u64,
    pub source: Pubkey,
    pub amount: u64,
    pub rent_refund: Pubkey,
    pub bump: u8,
}
