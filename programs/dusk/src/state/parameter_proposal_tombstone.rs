use anchor_lang::prelude::*;

/// Permanent identity after all support and excess rent are released. Keeping
/// the authenticated market, proposer, nonce, digest and terminal outcome
/// prevents address reuse and permits recovery-vote cleanup after release.
#[account]
#[derive(InitSpace)]
pub struct ParameterProposalTombstone {
    pub market: Pubkey,
    pub proposer: Pubkey,
    pub nonce: u64,
    pub digest: [u8; 32],
    pub status: super::ParameterProposalStatus,
    pub bump: u8,
}
