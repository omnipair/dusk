use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    state::{GovernanceAction, ProposalOrigin},
};

pub const GOVERNANCE_CONTROL_SEED: &[u8] = b"governance_control";
#[constant]
pub const MANAGER_POLICY_VERSION: u64 = 1;
#[constant]
pub const MANAGER_PENDING_LIMIT: u8 = 1;
#[constant]
pub const MANAGER_ANNOUNCEMENT_COOLDOWN: i64 = 3 * 24 * 60 * 60;

/// The only queued change that may execute in this market. Collecting LP
/// proposals reserve nothing. LP majority can replace a manager's pending entry.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace)]
pub struct PendingBundle {
    pub proposal: Pubkey,
    pub origin: ProposalOrigin,
    pub action: GovernanceAction,
    pub deadline: i64,
}

impl PendingBundle {
    pub fn active(&self, control: &GovernanceControl, clock: &Clock) -> Result<bool> {
        let authority_current = match self.origin {
            ProposalOrigin::Lp => match self.action {
                GovernanceAction::Parameters => true,
                GovernanceAction::AppointManager { expected_epoch, .. } => expected_epoch == control.manager_epoch,
            },
            ProposalOrigin::Manager { epoch, policy_version } => {
                control.manager != Pubkey::default()
                    && epoch == control.manager_epoch
                    && policy_version == MANAGER_POLICY_VERSION
            }
        };
        Ok(authority_current && self.origin.clock_value(clock)? <= self.deadline)
    }
}

#[account]
#[derive(InitSpace)]
pub struct GovernanceControl {
    pub market: Pubkey,
    pub manager: Pubkey,
    pub manager_epoch: u64,
    pub baseline_initialized: bool,
    /// Per-family LP-approved references, never an executable market snapshot.
    pub baseline_config: crate::state::MarketConfig,
    pub baseline_tranche_policy: crate::state::TranchePolicy,
    pub pending: Option<PendingBundle>,
    pub has_manager_announcement: bool,
    pub last_manager_announcement: i64,
    pub bump: u8,
}

impl GovernanceControl {
    pub fn initialize_if_needed(&mut self, market: Pubkey, bump: u8) -> Result<()> {
        if self.market == Pubkey::default() {
            self.market = market;
            self.bump = bump;
        }
        require_keys_eq!(self.market, market, ErrorCode::InvalidMarket);
        require_eq!(self.bump, bump, ErrorCode::InvalidArgument);
        Ok(())
    }

    pub fn prune(&mut self, clock: &Clock) -> Result<()> {
        if let Some(pending) = self.pending {
            if !pending.active(self, clock)? {
                self.pending = None;
            }
        }
        Ok(())
    }

    pub fn queue_lp(&mut self, pending: PendingBundle, clock: &Clock) -> Result<()> {
        self.prune(clock)?;
        require!(
            pending.origin == ProposalOrigin::Lp,
            ErrorCode::InvalidParameterProposal
        );
        if let Some(current) = self.pending {
            require!(
                matches!(current.origin, ProposalOrigin::Manager { .. }),
                ErrorCode::PendingBundleExists
            );
        }
        self.pending = Some(pending);
        Ok(())
    }

    pub fn queue_manager(&mut self, pending: PendingBundle, clock: &Clock) -> Result<()> {
        self.prune(clock)?;
        require!(self.pending.is_none(), ErrorCode::PendingBundleExists);
        require!(
            matches!(pending.origin, ProposalOrigin::Manager { .. }) && pending.active(self, clock)?,
            ErrorCode::ManagerEpochMismatch
        );
        if self.has_manager_announcement {
            require_gte!(
                clock.unix_timestamp,
                self.last_manager_announcement
                    .checked_add(MANAGER_ANNOUNCEMENT_COOLDOWN)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                ErrorCode::ManagerAnnouncementCooldown
            );
        }
        self.pending = Some(pending);
        self.has_manager_announcement = true;
        self.last_manager_announcement = clock.unix_timestamp;
        Ok(())
    }

    pub fn assert_pending(&self, proposal: Pubkey, clock: &Clock) -> Result<()> {
        let pending = self.pending.ok_or(ErrorCode::ProposalNotQueued)?;
        require!(
            pending.proposal == proposal && pending.active(self, clock)?,
            ErrorCode::ProposalStale
        );
        Ok(())
    }

    pub fn release(&mut self, proposal: Pubkey) {
        if self.pending.is_some_and(|pending| pending.proposal == proposal) {
            self.pending = None;
        }
    }

    pub fn replace_manager(&mut self, manager: Pubkey, expected_epoch: u64) -> Result<()> {
        require_eq!(self.manager_epoch, expected_epoch, ErrorCode::ManagerEpochMismatch);
        self.manager_epoch = self.manager_epoch.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
        self.manager = manager;
        // Authority changes invalidate manager announcements and appointments
        // endorsed for a previous term. Ordinary LP parameter bundles survive.
        if self.pending.is_some_and(|pending| {
            matches!(pending.origin, ProposalOrigin::Manager { .. })
                || matches!(pending.action, GovernanceAction::AppointManager { .. })
        }) {
            self.pending = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/state/governance_control.rs");
}
