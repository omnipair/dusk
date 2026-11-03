use super::*;
use crate::state::MarketConfig;
use crate::state::{encumber_recovery, recovery_locked, RecoveryLock, MAX_RECOVERY_LOCKS};

fn control() -> GovernanceControl {
    GovernanceControl {
        market: Pubkey::new_unique(), manager: Pubkey::new_unique(), manager_epoch: 1,
        baseline_initialized: true, baseline_config: MarketConfig::default(),
        baseline_insurance_principal_coverage_bps: 0, baseline_insurance_per_day_bps: 0,
        baseline_tranche_policy: Default::default(), baseline_junior_request_fee_bps: 0,
        pending: None, has_manager_announcement: false, last_manager_announcement: 0, bump: 1,
    }
}
fn pending(origin: ProposalOrigin, deadline: i64) -> PendingBundle {
    PendingBundle { proposal: Pubkey::new_unique(), origin, action: GovernanceAction::Parameters, deadline }
}
fn manager_pending(deadline: i64) -> PendingBundle {
    pending(ProposalOrigin::Manager { epoch: 1, policy_version: MANAGER_POLICY_VERSION }, deadline)
}
fn clock(unix_timestamp: i64, slot: u64) -> Clock {
    Clock { unix_timestamp, slot, ..Clock::default() }
}

#[test]
fn lp_majority_preempts_manager_but_nothing_preempts_pending_lp() {
    let mut state = control();
    let first = manager_pending(100);
    state.queue_manager(first, &clock(0, 0)).unwrap();
    let lp = pending(ProposalOrigin::Lp, i64::MAX);
    state.queue_lp(lp, &clock(1, 1)).unwrap();
    assert!(state.assert_pending(first.proposal, &clock(1, 1)).is_err());
    assert!(state.assert_pending(lp.proposal, &clock(1, 1)).is_ok());
    assert!(state.queue_lp(pending(ProposalOrigin::Lp, 100), &clock(1, 1)).is_err());
    assert!(state.queue_manager(manager_pending(100), &clock(MANAGER_ANNOUNCEMENT_COOLDOWN, 1)).is_err());
    // Resolving the replaced manager proposal cannot clear its LP successor.
    state.release(first.proposal);
    assert_eq!(state.pending.unwrap().proposal, lp.proposal);
}

#[test]
fn deadlines_use_origin_clock_and_expired_capacity_needs_no_keeper() {
    let mut state = control();
    let first = manager_pending(100);
    state.queue_manager(first, &clock(0, 0)).unwrap();
    assert!(state.assert_pending(first.proposal, &clock(i64::MAX, 100)).is_ok());
    assert!(state.assert_pending(first.proposal, &clock(0, 101)).is_err());
    let lp = pending(ProposalOrigin::Lp, 10);
    state.queue_lp(lp, &clock(0, 101)).unwrap();
    assert!(state.assert_pending(lp.proposal, &clock(10, u64::MAX)).is_ok());
    let next = pending(ProposalOrigin::Lp, 100);
    state.queue_lp(next, &clock(11, 0)).unwrap();
    assert_eq!(state.pending.unwrap().proposal, next.proposal);
}

#[test]
fn cancellation_and_rotation_preserve_announcement_cooldown() {
    let mut state = control();
    let first = manager_pending(i64::MAX);
    state.queue_manager(first, &clock(0, 0)).unwrap();
    assert!(state.queue_manager(manager_pending(i64::MAX), &clock(MANAGER_ANNOUNCEMENT_COOLDOWN, 0)).is_err());
    state.release(first.proposal);
    state.replace_manager(Pubkey::new_unique(), 1).unwrap();
    let next = pending(ProposalOrigin::Manager { epoch: 2, policy_version: MANAGER_POLICY_VERSION }, i64::MAX);
    assert!(state.queue_manager(next, &clock(MANAGER_ANNOUNCEMENT_COOLDOWN - 1, 0)).is_err());
    state.queue_manager(next, &clock(MANAGER_ANNOUNCEMENT_COOLDOWN, 0)).unwrap();
}

#[test]
fn removal_cancels_manager_bundle_and_preserves_lp_parameter_bundle() {
    let mut state = control();
    state.queue_manager(manager_pending(100), &clock(0, 0)).unwrap();
    state.replace_manager(Pubkey::default(), 1).unwrap();
    assert!(state.pending.is_none());
    assert_eq!(state.manager, Pubkey::default());
    let lp = pending(ProposalOrigin::Lp, 100);
    state.queue_lp(lp, &clock(0, 0)).unwrap();
    state.replace_manager(Pubkey::new_unique(), 2).unwrap();
    assert_eq!(state.pending.unwrap().proposal, lp.proposal);
    state.release(lp.proposal);
    let mut election = pending(ProposalOrigin::Lp, 100);
    election.action = GovernanceAction::AppointManager { manager: Pubkey::new_unique(), expected_epoch: 3 };
    state.queue_lp(election, &clock(0, 0)).unwrap();
    state.replace_manager(Pubkey::default(), 3).unwrap();
    assert!(state.pending.is_none());
}

#[test]
fn exact_control_allocation_stays_below_anchor_initialization_limit() {
    let mut state = control();
    let mut election = manager_pending(100);
    election.action = GovernanceAction::AppointManager { manager: Pubkey::new_unique(), expected_epoch: 1 };
    state.pending = Some(election);
    assert_eq!(state.try_to_vec().unwrap().len(), GovernanceControl::INIT_SPACE);
    assert!(GovernanceControl::INIT_SPACE + 8 <= 10_240);
}

#[test]
fn recovery_ownership_is_encumbered_once_per_ballot_with_finite_expiry() {
    let mut locks = [RecoveryLock::default(); MAX_RECOVERY_LOCKS];
    let first = Pubkey::new_unique();
    let second = Pubkey::new_unique();
    encumber_recovery(&mut locks, first, 0, 60, 10, 0).unwrap();
    encumber_recovery(&mut locks, second, 0, 80, 20, 0).unwrap();
    assert_eq!(recovery_locked(&locks, 0), 80);
    assert!(encumber_recovery(&mut locks, first, 0, 59, 10, 0).is_err());
    encumber_recovery(&mut locks, first, 0, 100, 10, 0).unwrap();
    assert_eq!(recovery_locked(&locks, 9), 100);
    assert_eq!(recovery_locked(&locks, 10), 80);
    assert_eq!(recovery_locked(&locks, 20), 0);
    encumber_recovery(&mut locks, first, 1, 50, 30, 20).unwrap();
    assert_eq!(recovery_locked(&locks, 20), 50);
}
