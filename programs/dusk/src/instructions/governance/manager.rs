use crate::state::ParameterProposalStatus;
use anchor_lang::prelude::*;

#[event]
pub struct ProposalResolved {
    pub proposal: Pubkey,
    pub market: Pubkey,
    pub status: ParameterProposalStatus,
    pub resolved_at: i64,
}

#[event]
pub struct ManagerChanged {
    pub market: Pubkey,
    pub manager: Pubkey,
    pub epoch: u64,
}
