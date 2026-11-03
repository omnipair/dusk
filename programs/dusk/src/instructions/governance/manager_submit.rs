use super::{save_released_proposal, validate_market_pda, ProposalResolved};
use crate::{account::get_size_with_discriminator, constants::*, errors::ErrorCode, state::*};
use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateManagerProposalArgs {
    pub nonce: u64,
    pub expected_epoch: u64,
    pub policy_version: u64,
    pub updates: Vec<MarketParameterUpdate>,
    pub metadata: ProposalMetadataV1,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(nonce: u64)]
pub struct CreateManagerProposal<'info> {
    /// A distinct funded signer permits a program-derived manager to compose
    /// proposals without requiring its authority account to be a System payer.
    #[account(mut)]
    pub payer: Signer<'info>,
    pub manager: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(mut, seeds = [GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump = governance_control.bump, has_one = market)]
    pub governance_control: Box<Account<'info, GovernanceControl>>,
    #[account(init, payer = payer, space = get_size_with_discriminator::<ParameterProposal>(), seeds = [PARAMETER_PROPOSAL_SEED_PREFIX, market.key().as_ref(), manager.key().as_ref(), &nonce.to_le_bytes()], bump)]
    pub proposal: Box<Account<'info, ParameterProposal>>,
    pub system_program: Program<'info, System>,
    #[account(address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Exact canonical custody, including authenticated zero custody before hLP initialization.
    pub base_hlp_ylp_vault: UncheckedAccount<'info>,
    /// CHECK: Exact canonical custody, including authenticated zero custody before hLP initialization.
    pub quote_hlp_ylp_vault: UncheckedAccount<'info>,
    /// CHECK: Canonical removal ballot cannot be omitted to bypass current LP authorization.
    #[account(seeds = [RECOVERY_BALLOT_SEED, market.key().as_ref(), RECOVERY_REMOVAL_KEY.as_ref()], bump)]
    pub removal_ballot: UncheckedAccount<'info>,
}

impl CreateManagerProposal<'_> {
    pub fn handle(ctx: Context<Self>, args: CreateManagerProposalArgs) -> Result<()> {
        validate_market_pda(&ctx.accounts.market, ctx.accounts.market.key())?;
        require_keys_eq!(
            ctx.accounts.governance_control.manager,
            ctx.accounts.manager.key(),
            ErrorCode::InvalidManager
        );
        require!(
            ctx.accounts.manager.key() != Pubkey::default(),
            ErrorCode::InvalidManager
        );
        require_eq!(
            args.expected_epoch,
            ctx.accounts.governance_control.manager_epoch,
            ErrorCode::ManagerEpochMismatch
        );
        require_eq!(
            args.policy_version,
            MANAGER_POLICY_VERSION,
            ErrorCode::ManagerEpochMismatch
        );
        let clock = Clock::get()?;
        let eligible = super::governance_eligible_from_accounts(
            &ctx.accounts.market,
            &ctx.accounts.ylp_mint,
            &ctx.accounts.base_hlp_ylp_vault,
            &ctx.accounts.quote_hlp_ylp_vault,
        )?;
        if let Some(ballot) = super::recovery::read_canonical_ballot(
            &ctx.accounts.removal_ballot.to_account_info(),
            ctx.accounts.market.key(),
            RecoveryTarget::RemoveManager {
                epoch: args.expected_epoch,
            },
        )? {
            require!(
                !ballot.passed
                    && !(ballot.window_active(clock.unix_timestamp)
                        && super::recovery::ballot_has_majority(&ballot, &ctx.accounts.market, eligible)?),
                ErrorCode::ManagerEpochMismatch
            );
        }
        ctx.accounts
            .market
            .assert_launch_settings_unchanged_after_start(&args.updates, clock.unix_timestamp)?;
        // All field/mode permissions and numerical limits come from the fixed protocol policy, never caller arguments.
        ctx.accounts
            .governance_control
            .validate_manager_updates(&ctx.accounts.market, &args.updates)?;
        let proposal = &mut ctx.accounts.proposal;
        proposal.initialize(
            ctx.accounts.market.key(),
            ctx.accounts.manager.key(),
            args.nonce,
            ctx.accounts.market.tranches.generation,
            &ctx.accounts.market.parameter_revisions,
            args.updates,
            GovernanceAction::Parameters,
            Vec::new(),
            args.metadata,
            1,
            clock.unix_timestamp,
            ctx.bumps.proposal,
        )?;
        proposal.origin = ProposalOrigin::Manager {
            epoch: args.expected_epoch,
            policy_version: args.policy_version,
        };
        proposal.rent_refund = ctx.accounts.payer.key();
        proposal.sponsorship_floor = 0;
        proposal.status = ParameterProposalStatus::Queued;
        proposal.queued_at = proposal.origin.clock_value(&clock)?;
        proposal.execute_after = proposal
            .queued_at
            .checked_add(i64::try_from(MANAGER_NOTICE_SLOTS).map_err(|_| ErrorCode::MarketMathOverflow)?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        proposal.execution_deadline = proposal
            .execute_after
            .checked_add(i64::try_from(MANAGER_EXECUTION_WINDOW_SLOTS).map_err(|_| ErrorCode::MarketMathOverflow)?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        proposal.refresh_intent_digest()?;
        proposal.seal_execution(&ctx.accounts.market.parameter_revisions)?;
        ctx.accounts
            .governance_control
            .queue_manager(proposal.pending_bundle(proposal.key()), &clock)?;
        emit_cpi!(super::created_event(proposal, proposal.key(), 0));
        emit_cpi!(super::queued_event(proposal, proposal.key()));
        Ok(())
    }
}

#[event_cpi]
#[derive(Accounts)]
pub struct ResolveManagerProposal<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(mut, seeds = [GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump = governance_control.bump, has_one = market)]
    pub governance_control: Box<Account<'info, GovernanceControl>>,
    /// CHECK: Owner, complete identity, origin, digest and canonical PDA checked below.
    #[account(mut)]
    pub proposal: UncheckedAccount<'info>,
    /// CHECK: Original stored rent payer, validated before refund.
    #[account(mut)]
    pub rent_refund: UncheckedAccount<'info>,
    pub manager: Option<Signer<'info>>,
}

impl ResolveManagerProposal<'_> {
    pub fn handle(ctx: Context<Self>, cancel: bool) -> Result<ParameterProposalStatus> {
        validate_market_pda(&ctx.accounts.market, ctx.accounts.market.key())?;
        let info = ctx.accounts.proposal.to_account_info();
        require_keys_eq!(*info.owner, crate::ID, ErrorCode::InvalidParameterProposal);
        let mut proposal = ParameterProposal::try_deserialize(&mut &info.try_borrow_data()?[..])?;
        proposal.assert_account(ctx.accounts.market.key(), *info.key)?;
        require!(
            matches!(proposal.origin, ProposalOrigin::Manager { .. }),
            ErrorCode::InvalidManager
        );
        require_keys_eq!(
            proposal.rent_refund,
            ctx.accounts.rent_refund.key(),
            ErrorCode::InvalidSigner
        );
        let clock = Clock::get()?;
        let now = clock.unix_timestamp;
        let previous = proposal.status;
        proposal.status = proposal.effective_status(
            &ctx.accounts.governance_control,
            &ctx.accounts.market.parameter_revisions,
            *info.key,
            &clock,
        );
        if cancel {
            let manager = ctx.accounts.manager.as_ref().ok_or(ErrorCode::InvalidManager)?;
            require_keys_eq!(manager.key(), proposal.proposer, ErrorCode::InvalidManager);
            require!(!proposal.terminal(), ErrorCode::RecoveryBallotInactive);
            proposal.status = ParameterProposalStatus::Cancelled;
        }
        require!(proposal.terminal(), ErrorCode::ProposalNotQueued);
        ctx.accounts.governance_control.release(*info.key);
        if previous != proposal.status {
            emit_cpi!(ProposalResolved {
                proposal: *info.key,
                market: proposal.market,
                status: proposal.status,
                resolved_at: now
            });
        }
        save_released_proposal(&proposal, &info, &ctx.accounts.rent_refund.to_account_info())?;
        Ok(proposal.status)
    }
}
