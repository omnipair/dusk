use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use super::{governance_eligible_from_accounts, validate_market_pda};
use crate::{account::get_size_with_discriminator, constants::*, errors::ErrorCode, state::*};

#[event]
pub struct RecoveryBallotChanged {
    pub ballot: Pubkey,
    pub market: Pubkey,
    pub target: RecoveryTarget,
    pub round: u64,
    pub senior_votes: u64,
    pub junior_receipt_votes: u64,
    pub junior_generation: u64,
    pub ends_at: i64,
    pub passed: bool,
    pub passed_slot: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(target: RecoveryTarget)]
pub struct OpenRecoveryBallot<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(seeds = [GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump = governance_control.bump, has_one = market)]
    pub governance_control: Box<Account<'info, GovernanceControl>>,
    #[account(init_if_needed, payer = payer, space = get_size_with_discriminator::<RecoveryBallot>(), seeds = [RECOVERY_BALLOT_SEED, market.key().as_ref(), target.seed_key().as_ref()], bump)]
    pub ballot: Box<Account<'info, RecoveryBallot>>,
    /// CHECK: Exact target identity, owner, canonical PDA, digest and status are validated below.
    pub target_proposal: Option<UncheckedAccount<'info>>,
    pub system_program: Program<'info, System>,
}

impl OpenRecoveryBallot<'_> {
    pub fn handle(ctx: Context<Self>, target: RecoveryTarget) -> Result<()> {
        validate_market_pda(&ctx.accounts.market, ctx.accounts.market.key())?;
        let clock = Clock::get()?;
        let now = clock.unix_timestamp;
        active_target(
            target,
            ctx.accounts
                .target_proposal
                .as_ref()
                .map(|account| account.to_account_info()),
            &ctx.accounts.market,
            &ctx.accounts.governance_control,
            &clock,
        )?;
        let ballot = &mut ctx.accounts.ballot;
        if ballot.market != Pubkey::default() {
            require_keys_eq!(ballot.market, ctx.accounts.market.key(), ErrorCode::InvalidMarket);
            // Preserve the previous decision's release boundary before a
            // reusable removal ballot advances to another manager term.
            require!(
                !ballot.passed || clock.slot > ballot.passed_slot,
                ErrorCode::RecoveryVoteLocked
            );
            if ballot.target == target {
                require!(!ballot.passed, ErrorCode::RecoveryBallotInactive);
                if ballot.window_active(now) {
                    return Ok(());
                }
                require_gte!(now, ballot.ends_at, ErrorCode::RecoveryBallotInactive);
            } else {
                require!(
                    matches!((ballot.target, target), (RecoveryTarget::RemoveManager {epoch: old}, RecoveryTarget::RemoveManager {epoch: next}) if old < next),
                    ErrorCode::RecoveryBallotInactive
                );
            }
            ballot.round = ballot.round.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
        }
        ballot.market = ctx.accounts.market.key();
        ballot.target = target;
        ballot.junior_generation = ctx.accounts.market.tranches.generation;
        ballot.senior_votes = 0;
        ballot.junior_receipt_votes = 0;
        ballot.opened_at = now;
        ballot.ends_at = now
            .checked_add(RECOVERY_BALLOT_WINDOW)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        ballot.passed = false;
        ballot.passed_slot = 0;
        ballot.bump = ctx.bumps.ballot;
        emit_cpi!(ballot_event(ballot.key(), ballot));
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct VoteRecoveryBallotArgs {
    pub round: u64,
    pub amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: VoteRecoveryBallotArgs)]
pub struct VoteRecoveryBallot<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub authority: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(mut, seeds = [GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump = governance_control.bump, has_one = market)]
    pub governance_control: Box<Account<'info, GovernanceControl>>,
    #[account(mut, seeds = [RECOVERY_BALLOT_SEED, market.key().as_ref(), ballot.target.seed_key().as_ref()], bump = ballot.bump, has_one = market)]
    pub ballot: Box<Account<'info, RecoveryBallot>>,
    /// CHECK: A canonical GovernanceBalance or direct ProposalSupport authenticated by the handler.
    #[account(mut)]
    pub source: UncheckedAccount<'info>,
    pub source_proposal: Option<Box<Account<'info, ParameterProposal>>>,
    /// CHECK: Exact cancellation target authenticated before any vote is counted; absent for removal.
    #[account(mut)]
    pub target_proposal: Option<UncheckedAccount<'info>>,
    #[account(init_if_needed, payer = payer, space = get_size_with_discriminator::<RecoveryVote>(), seeds = [RECOVERY_VOTE_SEED, ballot.key().as_ref(), &args.round.to_le_bytes(), source.key().as_ref()], bump)]
    pub vote: Box<Account<'info, RecoveryVote>>,
    #[account(address = market.srlp_mint)]
    pub srlp_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Canonical custody, or zero custody before hLP initialization, authenticated by the handler.
    pub base_hlp_srlp_vault: UncheckedAccount<'info>,
    /// CHECK: Canonical custody, or zero custody before hLP initialization, authenticated by the handler.
    pub quote_hlp_srlp_vault: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

impl VoteRecoveryBallot<'_> {
    pub fn handle(ctx: Context<Self>, args: VoteRecoveryBallotArgs) -> Result<()> {
        let clock = Clock::get()?;
        let now = clock.unix_timestamp;
        let market = &ctx.accounts.market;
        let eligible = governance_eligible_from_accounts(
            market,
            &ctx.accounts.srlp_mint,
            &ctx.accounts.base_hlp_srlp_vault,
            &ctx.accounts.quote_hlp_srlp_vault,
        )?;
        let ballot_key = ctx.accounts.ballot.key();
        let ballot = &mut ctx.accounts.ballot;
        require!(
            ballot.window_active(now) && ballot.round == args.round && args.amount > 0,
            ErrorCode::RecoveryBallotInactive
        );
        active_target(
            ballot.target,
            ctx.accounts
                .target_proposal
                .as_ref()
                .map(|account| account.to_account_info()),
            market,
            &ctx.accounts.governance_control,
            &clock,
        )?;
        let vote = &mut ctx.accounts.vote;
        if vote.ballot == Pubkey::default() {
            vote.ballot = ballot_key;
            vote.round = args.round;
            vote.source = ctx.accounts.source.key();
            vote.rent_refund = ctx.accounts.payer.key();
            vote.bump = ctx.bumps.vote;
        }
        require!(
            vote.ballot == ballot_key && vote.round == args.round && vote.source == ctx.accounts.source.key(),
            ErrorCode::InvalidProposalSupport
        );
        let next = vote
            .amount
            .checked_add(args.amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let kind = encumber_source(
            &ctx.accounts.source.to_account_info(),
            ctx.accounts.source_proposal.as_deref(),
            market,
            ctx.accounts.authority.key(),
            ballot_key,
            args.round,
            next,
            ballot.ends_at,
            now,
        )?;
        vote.amount = next;
        if ballot.junior_generation != market.tranches.generation {
            ballot.junior_generation = market.tranches.generation;
            ballot.junior_receipt_votes = 0;
        }
        let tally = match kind {
            GovernanceTokenKind::Senior => &mut ballot.senior_votes,
            GovernanceTokenKind::Junior => &mut ballot.junior_receipt_votes,
        };
        *tally = tally.checked_add(args.amount).ok_or(ErrorCode::MarketMathOverflow)?;
        if refresh_ballot(
            ballot,
            ctx.accounts
                .target_proposal
                .as_ref()
                .map(|account| account.to_account_info()),
            market,
            &mut ctx.accounts.governance_control,
            eligible,
            &clock,
        )? {
            match ballot.target {
                RecoveryTarget::RemoveManager { .. } => emit_cpi!(super::ManagerChanged {
                    market: market.key(),
                    manager: Pubkey::default(),
                    epoch: ctx.accounts.governance_control.manager_epoch
                }),
                RecoveryTarget::CancelProposal { proposal, .. } => emit_cpi!(super::ProposalResolved {
                    proposal,
                    market: market.key(),
                    status: ParameterProposalStatus::Cancelled,
                    resolved_at: now
                }),
            }
        }
        emit_cpi!(ballot_event(ballot_key, ballot));
        Ok(())
    }
}

#[derive(Accounts)]
pub struct ReleaseRecoveryVote<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(seeds = [GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump = governance_control.bump, has_one = market)]
    pub governance_control: Box<Account<'info, GovernanceControl>>,
    #[account(seeds = [RECOVERY_BALLOT_SEED, market.key().as_ref(), ballot.target.seed_key().as_ref()], bump = ballot.bump, has_one = market)]
    pub ballot: Box<Account<'info, RecoveryBallot>>,
    /// CHECK: Address bound to the authenticated vote. Only its matching encumbrance is removed.
    #[account(mut)]
    pub source: UncheckedAccount<'info>,
    /// CHECK: Authenticated target when needed to prove early termination.
    pub target_proposal: Option<UncheckedAccount<'info>>,
    #[account(mut, close = rent_refund, seeds = [RECOVERY_VOTE_SEED, ballot.key().as_ref(), &vote.round.to_le_bytes(), source.key().as_ref()], bump = vote.bump, constraint = vote.ballot == ballot.key() && vote.source == source.key() @ ErrorCode::InvalidProposalSupport)]
    pub vote: Box<Account<'info, RecoveryVote>>,
    /// CHECK: Exact original vote rent payer; never redirected by a delegate or caller.
    #[account(mut, address = vote.rent_refund)]
    pub rent_refund: UncheckedAccount<'info>,
}

impl ReleaseRecoveryVote<'_> {
    pub fn handle(ctx: Context<Self>) -> Result<()> {
        validate_market_pda(&ctx.accounts.market, ctx.accounts.market.key())?;
        let ballot = &ctx.accounts.ballot;
        let vote = &ctx.accounts.vote;
        let clock = Clock::get()?;
        let now = clock.unix_timestamp;
        require!(
            ballot.round != vote.round || !ballot.passed || clock.slot > ballot.passed_slot,
            ErrorCode::RecoveryVoteLocked
        );
        let still_active = if ballot.round == vote.round && ballot.window_active(now) {
            inspect_target(
                ballot.target,
                ctx.accounts
                    .target_proposal
                    .as_ref()
                    .map(|account| account.to_account_info()),
                &ctx.accounts.market,
                &ctx.accounts.governance_control,
                &clock,
            )?
        } else {
            false
        };
        require!(!still_active, ErrorCode::RecoveryVoteLocked);
        let info = ctx.accounts.source.to_account_info();
        if *info.owner == crate::ID {
            let decoded_balance = {
                let data = info.try_borrow_data()?;
                GovernanceBalance::try_deserialize(&mut &data[..])
            };
            if let Ok(mut balance) = decoded_balance {
                clear_lock(&mut balance.recovery_locks, ballot.key(), vote.round);
                balance.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
            } else {
                let mut support = ProposalSupport::try_deserialize(&mut &info.try_borrow_data()?[..])?;
                clear_lock(&mut support.recovery_locks, ballot.key(), vote.round);
                support.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
            }
        } else {
            require!(
                info.data_is_empty() && *info.owner == System::id(),
                ErrorCode::InvalidProposalSupport
            );
        }
        Ok(())
    }
}

fn clear_lock(locks: &mut [RecoveryLock; MAX_RECOVERY_LOCKS], ballot: Pubkey, round: u64) {
    for lock in locks
        .iter_mut()
        .filter(|lock| lock.ballot == ballot && lock.round == round)
    {
        *lock = RecoveryLock::default();
    }
}

#[inline(never)]
fn encumber_source(
    info: &AccountInfo,
    direct: Option<&Account<ParameterProposal>>,
    market: &Account<Market>,
    authority: Pubkey,
    ballot: Pubkey,
    round: u64,
    amount: u64,
    until: i64,
    now: i64,
) -> Result<GovernanceTokenKind> {
    require_keys_eq!(*info.owner, crate::ID, ErrorCode::InvalidProposalSupport);
    let decoded_balance = {
        let data = info.try_borrow_data()?;
        GovernanceBalance::try_deserialize(&mut &data[..])
    };
    if let Ok(mut balance) = decoded_balance {
        balance.assert_account(market.key(), *info.key)?;
        balance.authorize(authority)?;
        require_gte!(balance.total_amount, amount, ErrorCode::InsufficientBalance);
        let kind = GovernanceTokenKind::from_code(balance.token_kind)?;
        let expected_mint = match kind {
            GovernanceTokenKind::Senior => market.srlp_mint,
            GovernanceTokenKind::Junior => market.junior_mint,
        };
        require_keys_eq!(balance.lp_mint, expected_mint, ErrorCode::InvalidLpMintKey);
        encumber_recovery(&mut balance.recovery_locks, ballot, round, amount, until, now)?;
        balance.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
        Ok(kind)
    } else {
        let mut support = ProposalSupport::try_deserialize(&mut &info.try_borrow_data()?[..])?;
        let proposal = direct.ok_or(ErrorCode::InvalidParameterProposal)?;
        proposal.assert_account(market.key(), proposal.key())?;
        require!(
            support.proposal == proposal.key() && support.supporter == authority,
            ErrorCode::InvalidSigner
        );
        let expected = Pubkey::create_program_address(
            &[
                PROPOSAL_SUPPORT_SEED_PREFIX,
                proposal.key().as_ref(),
                authority.as_ref(),
                &[support.bump],
            ],
            &crate::ID,
        )
        .map_err(|_| error!(ErrorCode::InvalidProposalSupport))?;
        require_keys_eq!(*info.key, expected, ErrorCode::InvalidProposalSupport);
        require_gte!(support.locked_amount, amount, ErrorCode::InsufficientBalance);
        encumber_recovery(&mut support.recovery_locks, ballot, round, amount, until, now)?;
        support.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
        Ok(GovernanceTokenKind::Senior)
    }
}

fn active_target(
    target: RecoveryTarget,
    info: Option<AccountInfo>,
    market: &Account<Market>,
    control: &GovernanceControl,
    clock: &Clock,
) -> Result<()> {
    require!(
        inspect_target(target, info, market, control, clock)?,
        ErrorCode::RecoveryBallotInactive
    );
    Ok(())
}

fn inspect_target(
    target: RecoveryTarget,
    info: Option<AccountInfo>,
    market: &Account<Market>,
    control: &GovernanceControl,
    clock: &Clock,
) -> Result<bool> {
    match target {
        RecoveryTarget::RemoveManager { epoch } => {
            Ok(control.manager != Pubkey::default() && control.manager_epoch == epoch)
        }
        RecoveryTarget::CancelProposal { proposal: key, digest } => {
            let info = info.ok_or(ErrorCode::InvalidParameterProposal)?;
            require!(
                info.key == &key && *info.owner == crate::ID,
                ErrorCode::InvalidParameterProposal
            );
            let data = info.try_borrow_data()?;
            if let Ok(proposal) = ParameterProposal::try_deserialize(&mut &data[..]) {
                proposal.assert_account(market.key(), key)?;
                require!(proposal.digest == digest, ErrorCode::InvalidProposalDigest);
                // Only the market's one queued bundle can be vetoed. A
                // collecting proposal does not reserve or change the market.
                Ok(
                    proposal.effective_status(control, &market.parameter_revisions, key, clock)
                        == ParameterProposalStatus::Queued,
                )
            } else {
                let tombstone = ParameterProposalTombstone::try_deserialize(&mut &data[..])?;
                require!(
                    tombstone.market == market.key()
                        && tombstone.digest == digest
                        && RecoveryBallot::terminal_target(tombstone.status),
                    ErrorCode::InvalidParameterProposal
                );
                let expected = Pubkey::create_program_address(
                    &[
                        PARAMETER_PROPOSAL_SEED_PREFIX,
                        tombstone.market.as_ref(),
                        tombstone.proposer.as_ref(),
                        &tombstone.nonce.to_le_bytes(),
                        &[tombstone.bump],
                    ],
                    &crate::ID,
                )
                .map_err(|_| error!(ErrorCode::InvalidParameterProposal))?;
                require_keys_eq!(expected, key, ErrorCode::InvalidParameterProposal);
                Ok(false)
            }
        }
    }
}

pub(super) fn ballot_event(key: Pubkey, ballot: &RecoveryBallot) -> RecoveryBallotChanged {
    RecoveryBallotChanged {
        ballot: key,
        market: ballot.market,
        target: ballot.target,
        round: ballot.round,
        senior_votes: ballot.senior_votes,
        junior_receipt_votes: ballot.junior_receipt_votes,
        junior_generation: ballot.junior_generation,
        ends_at: ballot.ends_at,
        passed: ballot.passed,
        passed_slot: ballot.passed_slot,
    }
}

pub(super) fn ballot_has_majority(ballot: &RecoveryBallot, market: &Market, eligible: u64) -> Result<bool> {
    let junior = if ballot.junior_generation == market.tranches.generation {
        junior_receipt_value(ballot.junior_receipt_votes, &market.tranches)?
    } else {
        0
    };
    let support = ballot
        .senior_votes
        .checked_add(junior)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    has_strict_support_majority(support, eligible)
}

pub(super) fn read_canonical_ballot(
    info: &AccountInfo,
    market: Pubkey,
    target: RecoveryTarget,
) -> Result<Option<RecoveryBallot>> {
    let (expected, bump) =
        Pubkey::find_program_address(&[RECOVERY_BALLOT_SEED, market.as_ref(), &target.seed_key()], &crate::ID);
    require_keys_eq!(*info.key, expected, ErrorCode::InvalidParameterProposal);
    if *info.owner == System::id() {
        require!(info.data_is_empty(), ErrorCode::InvalidParameterProposal);
        return Ok(None);
    }
    require_keys_eq!(*info.owner, crate::ID, ErrorCode::InvalidParameterProposal);
    let ballot = RecoveryBallot::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    require!(
        ballot.market == market && ballot.bump == bump,
        ErrorCode::InvalidParameterProposal
    );
    if ballot.target != target {
        require!(
            matches!((ballot.target, target), (RecoveryTarget::RemoveManager {epoch: old}, RecoveryTarget::RemoveManager {epoch: next}) if old < next),
            ErrorCode::InvalidParameterProposal
        );
        return Ok(None);
    }
    Ok(Some(ballot))
}

pub(super) fn refresh_ballot(
    ballot: &mut RecoveryBallot,
    target: Option<AccountInfo>,
    market: &Account<Market>,
    control: &mut GovernanceControl,
    eligible: u64,
    clock: &Clock,
) -> Result<bool> {
    if !ballot.window_active(clock.unix_timestamp) {
        return Ok(false);
    }
    if !inspect_target(ballot.target, target.clone(), market, control, clock)?
        || !ballot_has_majority(ballot, market, eligible)?
    {
        return Ok(false);
    }
    ballot.passed = true;
    ballot.passed_slot = clock.slot;
    match ballot.target {
        RecoveryTarget::RemoveManager { epoch } => control.replace_manager(Pubkey::default(), epoch)?,
        RecoveryTarget::CancelProposal { proposal: key, .. } => {
            let info = target.ok_or(ErrorCode::InvalidParameterProposal)?;
            let mut proposal = ParameterProposal::try_deserialize(&mut &info.try_borrow_data()?[..])?;
            proposal.status = ParameterProposalStatus::Cancelled;
            proposal.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
            control.release(key);
        }
    }
    Ok(true)
}

pub(super) fn refresh_canonical_ballot(
    info: &AccountInfo,
    target_info: Option<AccountInfo>,
    market: &Account<Market>,
    control: &mut GovernanceControl,
    target: RecoveryTarget,
    eligible: u64,
    clock: &Clock,
) -> Result<Option<RecoveryBallot>> {
    let Some(mut ballot) = read_canonical_ballot(info, market.key(), target)? else {
        return Ok(None);
    };
    if !refresh_ballot(&mut ballot, target_info, market, control, eligible, clock)? {
        return Ok(None);
    }
    ballot.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
    Ok(Some(ballot))
}

#[event_cpi]
#[derive(Accounts)]
pub struct RefreshRecoveryBallot<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(mut, seeds = [GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump = governance_control.bump, has_one = market)]
    pub governance_control: Box<Account<'info, GovernanceControl>>,
    #[account(mut, seeds = [RECOVERY_BALLOT_SEED, market.key().as_ref(), ballot.target.seed_key().as_ref()], bump = ballot.bump, has_one = market)]
    pub ballot: Box<Account<'info, RecoveryBallot>>,
    /// CHECK: Exact target identity and terminal status verified before applying any outcome.
    #[account(mut)]
    pub target_proposal: Option<UncheckedAccount<'info>>,
    #[account(address = market.srlp_mint)]
    pub srlp_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Canonical custody, or zero custody before hLP initialization, authenticated by the handler.
    pub base_hlp_srlp_vault: UncheckedAccount<'info>,
    /// CHECK: Canonical custody, or zero custody before hLP initialization, authenticated by the handler.
    pub quote_hlp_srlp_vault: UncheckedAccount<'info>,
}

impl RefreshRecoveryBallot<'_> {
    pub fn handle(ctx: Context<Self>) -> Result<bool> {
        let market = &ctx.accounts.market;
        let eligible = governance_eligible_from_accounts(
            market,
            &ctx.accounts.srlp_mint,
            &ctx.accounts.base_hlp_srlp_vault,
            &ctx.accounts.quote_hlp_srlp_vault,
        )?;
        let clock = Clock::get()?;
        let now = clock.unix_timestamp;
        let key = ctx.accounts.ballot.key();
        let ballot = &mut ctx.accounts.ballot;
        if refresh_ballot(
            ballot,
            ctx.accounts
                .target_proposal
                .as_ref()
                .map(|account| account.to_account_info()),
            market,
            &mut ctx.accounts.governance_control,
            eligible,
            &clock,
        )? {
            match ballot.target {
                RecoveryTarget::RemoveManager { .. } => emit_cpi!(super::ManagerChanged {
                    market: market.key(),
                    manager: Pubkey::default(),
                    epoch: ctx.accounts.governance_control.manager_epoch
                }),
                RecoveryTarget::CancelProposal { proposal, .. } => emit_cpi!(super::ProposalResolved {
                    proposal,
                    market: market.key(),
                    status: ParameterProposalStatus::Cancelled,
                    resolved_at: now
                }),
            }
            emit_cpi!(ballot_event(key, ballot));
        }
        Ok(ballot.passed)
    }
}
