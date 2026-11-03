use crate::{errors::ErrorCode, state::ParameterProposalStatus};
use anchor_lang::prelude::*;

pub const RECOVERY_BALLOT_SEED: &[u8] = b"recovery_ballot";
pub const RECOVERY_REMOVAL_KEY: [u8; 32] = [0; 32];
#[constant]
pub const RECOVERY_BALLOT_WINDOW: i64 = 7 * 24 * 60 * 60;
pub const MAX_RECOVERY_LOCKS: usize = 8;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace)]
pub struct RecoveryLock {
    pub ballot: Pubkey,
    pub round: u64,
    pub amount: u64,
    pub until: i64,
}

pub fn recovery_locked(locks: &[RecoveryLock; MAX_RECOVERY_LOCKS], now: i64) -> u64 {
    locks
        .iter()
        .filter(|lock| now < lock.until)
        .map(|lock| lock.amount)
        .max()
        .unwrap_or(0)
}

pub fn encumber_recovery(
    locks: &mut [RecoveryLock; MAX_RECOVERY_LOCKS],
    ballot: Pubkey,
    round: u64,
    amount: u64,
    until: i64,
    now: i64,
) -> Result<()> {
    require!(until > now && amount > 0, ErrorCode::RecoveryBallotInactive);
    let index = locks
        .iter()
        .position(|lock| lock.ballot == ballot && lock.round == round)
        .or_else(|| locks.iter().position(|lock| now >= lock.until))
        .ok_or(ErrorCode::GovernanceCapacityExceeded)?;
    let previous = locks[index];
    if previous.ballot == ballot && previous.round == round && now < previous.until {
        require_gte!(amount, previous.amount, ErrorCode::InvalidProposalSupport);
    }
    locks[index] = RecoveryLock {
        ballot,
        round,
        amount,
        until,
    };
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum RecoveryTarget {
    CancelProposal { proposal: Pubkey, digest: [u8; 32] },
    RemoveManager { epoch: u64 },
}

impl RecoveryTarget {
    pub fn seed_key(self) -> [u8; 32] {
        match self {
            Self::CancelProposal { proposal, .. } => proposal.to_bytes(),
            Self::RemoveManager { .. } => RECOVERY_REMOVAL_KEY,
        }
    }
}

#[account]
#[derive(InitSpace)]
pub struct RecoveryBallot {
    pub market: Pubkey,
    pub target: RecoveryTarget,
    pub round: u64,
    pub junior_generation: u64,
    pub senior_votes: u64,
    pub junior_receipt_votes: u64,
    pub opened_at: i64,
    pub ends_at: i64,
    pub passed: bool,
    pub bump: u8,
}

impl RecoveryBallot {
    pub fn window_active(&self, now: i64) -> bool {
        !self.passed && now >= self.opened_at && now < self.ends_at
    }

    pub fn terminal_target(status: ParameterProposalStatus) -> bool {
        !matches!(
            status,
            ParameterProposalStatus::Collecting | ParameterProposalStatus::Queued
        )
    }
}
