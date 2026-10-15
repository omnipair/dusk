use super::{
    direct_ylp_eligible_supply, validate_governance_token_accounts, validate_market_pda, WithdrawParameterSupportArgs,
};
use crate::{
    account::{get_size_with_discriminator, initialize_pda_account_if_needed},
    constants::*,
    errors::ErrorCode,
    events::{GovernanceSupportChanged, ParameterProposalCreated, ParameterProposalQueued},
    instructions::accounts::validate_lp_mint,
    state::{
        GovernanceBalance, GovernanceSupport, Market, MarketParameterUpdate, ParameterProposal,
        ParameterProposalStatus, ProposalMetadataV1,
    },
    token::create_token_account,
};
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateGovernanceProposalArgs {
    pub nonce: u64,
    pub updates: Vec<MarketParameterUpdate>,
    pub metadata: ProposalMetadataV1,
    /// One positive amount per remaining [balance, support] pair. Balance
    /// addresses must be strictly ascending; all owners authorize the signer.
    pub contributions: Vec<u64>,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(nonce: u64)]
pub struct CreateGovernanceProposal<'info> {
    #[account(mut)]
    pub proposer: Signer<'info>,

    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        init,
        payer = proposer,
        space = get_size_with_discriminator::<ParameterProposal>(),
        seeds = [
            PARAMETER_PROPOSAL_SEED_PREFIX,
            market.key().as_ref(),
            proposer.key().as_ref(),
            nonce.to_le_bytes().as_ref(),
        ],
        bump,
    )]
    pub proposal: Box<Account<'info, ParameterProposal>>,

    #[account(address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    /// CHECK: Canonical PDA. A System-owned empty account is initialized as a
    /// Token-2022 yLP vault by the handler; an existing vault is fully parsed
    /// and validated before proposal state changes.
    #[account(
        mut,
        seeds = [
            HLP_YLP_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            market.base_side.hlp_mint.as_ref(),
            ylp_mint.key().as_ref(),
        ],
        bump,
    )]
    pub base_hlp_ylp_vault: UncheckedAccount<'info>,

    /// CHECK: Validated and initialized under the same rules as the base-side
    /// hLP yLP vault above.
    #[account(
        mut,
        seeds = [
            HLP_YLP_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            market.quote_side.hlp_mint.as_ref(),
            ylp_mint.key().as_ref(),
        ],
        bump,
    )]
    pub quote_hlp_ylp_vault: UncheckedAccount<'info>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> CreateGovernanceProposal<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: CreateGovernanceProposalArgs) -> Result<()> {
        validate_market_pda(&ctx.accounts.market, ctx.accounts.market.key())?;
        validate_lp_mint(
            &ctx.accounts.ylp_mint,
            ctx.accounts.market.key(),
            ctx.accounts.market.base_side.asset_decimals,
        )?;
        ctx.accounts.market.validate_parameter_updates(&args.updates)?;
        let base_amount = super::proposal::governance_vault_amount(
            &ctx.accounts.market,
            &ctx.accounts.ylp_mint,
            &ctx.accounts.base_hlp_ylp_vault,
            ctx.accounts.market.base_hlp_vault.ylp_vault,
            ctx.accounts.market.base_hlp_vault.ylp_shares,
        )?;
        let quote_amount = super::proposal::governance_vault_amount(
            &ctx.accounts.market,
            &ctx.accounts.ylp_mint,
            &ctx.accounts.quote_hlp_ylp_vault,
            ctx.accounts.market.quote_hlp_vault.ylp_vault,
            ctx.accounts.market.quote_hlp_vault.ylp_shares,
        )?;
        let eligible_supply = direct_ylp_eligible_supply(
            &ctx.accounts.market,
            ctx.accounts.ylp_mint.supply,
            base_amount,
            quote_amount,
        )?;
        let market_key = ctx.accounts.market.key();
        let proposal_key = ctx.accounts.proposal.key();
        let clock = Clock::get()?;
        ctx.accounts.proposal.initialize(
            market_key,
            ctx.accounts.proposer.key(),
            args.nonce,
            &ctx.accounts.market.parameter_revisions,
            args.updates,
            args.metadata,
            eligible_supply,
            clock.unix_timestamp,
            ctx.bumps.proposal,
        )?;
        create_token_account(
            &ctx.accounts.market.to_account_info(),
            &ctx.accounts.proposer.to_account_info(),
            &ctx.accounts.base_hlp_ylp_vault.to_account_info(),
            &ctx.accounts.ylp_mint.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.token_2022_program.to_account_info(),
            &[
                HLP_YLP_VAULT_SEED_PREFIX,
                market_key.as_ref(),
                ctx.accounts.market.base_side.hlp_mint.as_ref(),
                ctx.accounts.ylp_mint.key().as_ref(),
                &[ctx.bumps.base_hlp_ylp_vault],
            ],
        )?;
        create_token_account(
            &ctx.accounts.market.to_account_info(),
            &ctx.accounts.proposer.to_account_info(),
            &ctx.accounts.quote_hlp_ylp_vault.to_account_info(),
            &ctx.accounts.ylp_mint.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.token_2022_program.to_account_info(),
            &[
                HLP_YLP_VAULT_SEED_PREFIX,
                market_key.as_ref(),
                ctx.accounts.market.quote_side.hlp_mint.as_ref(),
                ctx.accounts.ylp_mint.key().as_ref(),
                &[ctx.bumps.quote_hlp_ylp_vault],
            ],
        )?;

        let changes = commit_contributions(
            &mut ctx.accounts.proposal,
            proposal_key,
            ctx.accounts.proposer.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ctx.remaining_accounts,
            &args.contributions,
        )?;
        require_gte!(
            ctx.accounts.proposal.total_locked,
            ctx.accounts.proposal.sponsorship_floor,
            ErrorCode::ProposalSponsorshipTooLow
        );
        let queued = ctx
            .accounts
            .proposal
            .queue_if_supported(eligible_supply, clock.unix_timestamp)?;
        emit_cpi!(ParameterProposalCreated {
            proposal: proposal_key,
            market: market_key,
            proposer: ctx.accounts.proposer.key(),
            nonce: args.nonce,
            family_revisions: ctx.accounts.proposal.family_revisions,
            digest: ctx.accounts.proposal.digest,
            sponsorship_floor: ctx.accounts.proposal.sponsorship_floor,
            initial_support: ctx.accounts.proposal.total_locked,
            status: ctx.accounts.proposal.status.code(),
            updates: ctx.accounts.proposal.updates.clone(),
            metadata: ctx.accounts.proposal.metadata.clone(),
        });
        for mut change in changes {
            change.status = ctx.accounts.proposal.status.code();
            change.total_locked = ctx.accounts.proposal.total_locked;
            emit_cpi!(change);
        }
        if queued {
            emit_cpi!(queued_event(&ctx.accounts.proposal, proposal_key));
        }
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SupportGovernanceProposalArgs {
    pub digest: [u8; 32],
    /// Same canonical remaining-account pairs as creation.
    pub contributions: Vec<u64>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct SupportGovernanceProposal<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub proposal: Box<Account<'info, ParameterProposal>>,
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub quote_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub system_program: Program<'info, System>,
}

impl<'info> SupportGovernanceProposal<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: SupportGovernanceProposalArgs) -> Result<()> {
        validate_governance_token_accounts(
            &ctx.accounts.market,
            &ctx.accounts.ylp_mint,
            &ctx.accounts.base_hlp_ylp_vault,
            &ctx.accounts.quote_hlp_ylp_vault,
        )?;
        let proposal_key = ctx.accounts.proposal.key();
        ctx.accounts
            .proposal
            .assert_account(ctx.accounts.market.key(), proposal_key)?;
        require!(
            ctx.accounts.proposal.digest == args.digest,
            ErrorCode::ProposalDigestMismatch
        );
        require!(
            ctx.accounts.proposal.status == ParameterProposalStatus::Collecting,
            ErrorCode::ProposalNotCollecting
        );
        require!(
            ctx.accounts
                .proposal
                .revisions_current(&ctx.accounts.market.parameter_revisions),
            ErrorCode::ProposalStale
        );
        let eligible_supply = direct_ylp_eligible_supply(
            &ctx.accounts.market,
            ctx.accounts.ylp_mint.supply,
            ctx.accounts.base_hlp_ylp_vault.amount,
            ctx.accounts.quote_hlp_ylp_vault.amount,
        )?;
        let changes = commit_contributions(
            &mut ctx.accounts.proposal,
            proposal_key,
            ctx.accounts.authority.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ctx.remaining_accounts,
            &args.contributions,
        )?;
        let queued = ctx
            .accounts
            .proposal
            .queue_if_supported(eligible_supply, Clock::get()?.unix_timestamp)?;
        for mut change in changes {
            change.status = ctx.accounts.proposal.status.code();
            change.total_locked = ctx.accounts.proposal.total_locked;
            emit_cpi!(change);
        }
        if queued {
            emit_cpi!(queued_event(&ctx.accounts.proposal, proposal_key));
        }
        Ok(())
    }
}

/// Allocations never mutate token supply or virtual yield: the owner's balance
/// already holds those shares. Strict ordering rejects duplicate account aliases.
fn commit_contributions<'info>(
    proposal: &mut ParameterProposal,
    proposal_key: Pubkey,
    authority: AccountInfo<'info>,
    system_program: AccountInfo<'info>,
    accounts: &[AccountInfo<'info>],
    amounts: &[u64],
) -> Result<Vec<GovernanceSupportChanged>> {
    require!(
        !amounts.is_empty() && accounts.len() == amounts.len().checked_mul(2).ok_or(ErrorCode::InvalidArgument)?,
        ErrorCode::InvalidArgument
    );
    let mut previous: Option<Pubkey> = None;
    let mut changes = Vec::with_capacity(amounts.len());
    for (pair, amount) in accounts.chunks(2).zip(amounts.iter().copied()) {
        let balance_info = &pair[0];
        let support_info = &pair[1];
        require!(
            balance_info.is_writable && support_info.is_writable,
            ErrorCode::InvalidProposalSupport
        );
        if let Some(key) = previous {
            require!(key < *balance_info.key, ErrorCode::InvalidProposalSupport);
        }
        previous = Some(*balance_info.key);
        require_keys_eq!(*balance_info.owner, crate::ID, ErrorCode::InvalidProposalSupport);
        let mut balance = GovernanceBalance::try_deserialize(&mut &balance_info.try_borrow_data()?[..])?;
        balance.assert_account(proposal.market, *balance_info.key)?;
        balance.authorize(*authority.key)?;
        balance.commit(amount)?;
        let (expected, bump) = Pubkey::find_program_address(
            &[
                GOVERNANCE_SUPPORT_SEED_PREFIX,
                proposal_key.as_ref(),
                balance_info.key.as_ref(),
            ],
            &crate::ID,
        );
        require_keys_eq!(*support_info.key, expected, ErrorCode::InvalidProposalSupport);
        let initialized = initialize_pda_account_if_needed(
            authority.clone(),
            support_info.clone(),
            system_program.clone(),
            get_size_with_discriminator::<GovernanceSupport>(),
            &[
                GOVERNANCE_SUPPORT_SEED_PREFIX,
                proposal_key.as_ref(),
                balance_info.key.as_ref(),
                &[bump],
            ],
        )?;
        let mut support = if initialized {
            GovernanceSupport {
                proposal: proposal_key,
                governance_balance: *balance_info.key,
                amount: 0,
                rent_refund: *authority.key,
                bump,
            }
        } else {
            GovernanceSupport::try_deserialize(&mut &support_info.try_borrow_data()?[..])?
        };
        require_keys_eq!(support.proposal, proposal_key, ErrorCode::InvalidProposalSupport);
        require_keys_eq!(
            support.governance_balance,
            *balance_info.key,
            ErrorCode::InvalidProposalSupport
        );
        require_eq!(support.bump, bump, ErrorCode::InvalidProposalSupport);
        support.amount = support
            .amount
            .checked_add(amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        proposal.total_locked = proposal
            .total_locked
            .checked_add(amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        balance.try_serialize(&mut &mut balance_info.try_borrow_mut_data()?[..])?;
        support.try_serialize(&mut &mut support_info.try_borrow_mut_data()?[..])?;
        changes.push(GovernanceSupportChanged {
            proposal: proposal_key,
            governance_balance: *balance_info.key,
            owner: balance.owner,
            authority: *authority.key,
            withdrawn: false,
            amount,
            owner_support: support.amount,
            total_locked: proposal.total_locked,
            status: proposal.status.code(),
            proposal_tombstoned: false,
        });
    }
    Ok(changes)
}

fn queued_event(proposal: &ParameterProposal, key: Pubkey) -> ParameterProposalQueued {
    ParameterProposalQueued {
        proposal: key,
        total_locked: proposal.queued_support,
        eligible_supply: proposal.queued_eligible_ylp,
        queued_at: proposal.queued_at,
        execute_after: proposal.execute_after,
        execution_deadline: proposal.execution_deadline,
    }
}

#[event_cpi]
#[derive(Accounts)]
pub struct WithdrawGovernanceSupport<'info> {
    pub authority: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    /// CHECK: Owner, discriminator, canonical address, digest and proposer checked in handler; may become a tombstone.
    #[account(mut)]
    pub proposal: UncheckedAccount<'info>,
    /// CHECK: Validated against proposal rent payer; receives rent only.
    #[account(mut)]
    pub proposer: UncheckedAccount<'info>,
    #[account(mut)]
    pub governance_balance: Box<Account<'info, GovernanceBalance>>,
    #[account(mut, seeds = [GOVERNANCE_SUPPORT_SEED_PREFIX, proposal.key().as_ref(), governance_balance.key().as_ref()], bump = governance_support.bump)]
    pub governance_support: Box<Account<'info, GovernanceSupport>>,
    /// CHECK: Original support rent payer; cannot be redirected on delegate replacement.
    #[account(mut, address = governance_support.rent_refund)]
    pub rent_refund: UncheckedAccount<'info>,
}

impl<'info> WithdrawGovernanceSupport<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: WithdrawParameterSupportArgs) -> Result<()> {
        validate_market_pda(&ctx.accounts.market, ctx.accounts.market.key())?;
        let balance = &mut ctx.accounts.governance_balance;
        balance.assert_account(ctx.accounts.market.key(), balance.key())?;
        balance.authorize(ctx.accounts.authority.key())?;
        require_keys_eq!(
            *ctx.accounts.proposal.owner,
            crate::ID,
            ErrorCode::InvalidParameterProposal
        );
        let mut proposal = ParameterProposal::try_deserialize(&mut &ctx.accounts.proposal.try_borrow_data()?[..])?;
        proposal.assert_account(ctx.accounts.market.key(), ctx.accounts.proposal.key())?;
        require_keys_eq!(
            ctx.accounts.proposer.key(),
            proposal.proposer,
            ErrorCode::InvalidParameterProposal
        );
        let support = &mut ctx.accounts.governance_support;
        require_keys_eq!(
            support.proposal,
            ctx.accounts.proposal.key(),
            ErrorCode::InvalidProposalSupport
        );
        require_keys_eq!(
            support.governance_balance,
            balance.key(),
            ErrorCode::InvalidProposalSupport
        );
        proposal.mark_stale_if_revision_changed(&ctx.accounts.market.parameter_revisions);
        proposal.mark_expired_if_past_deadline(Clock::get()?.unix_timestamp);
        require!(
            proposal.status != ParameterProposalStatus::Queued,
            ErrorCode::ProposalSupportFrozen
        );
        let amount = args.amount.unwrap_or(support.amount);
        require!(amount > 0, ErrorCode::AmountZero);
        require_gte!(support.amount, amount, ErrorCode::InvalidProposalSupport);
        support.amount -= amount;
        balance.committed_amount = balance
            .committed_amount
            .checked_sub(amount)
            .ok_or(ErrorCode::InvalidProposalSupport)?;
        proposal.total_locked = proposal
            .total_locked
            .checked_sub(amount)
            .ok_or(ErrorCode::InvalidProposalSupport)?;
        proposal.cancel_if_below_sponsorship_floor();
        emit_cpi!(GovernanceSupportChanged {
            proposal: ctx.accounts.proposal.key(),
            governance_balance: balance.key(),
            owner: balance.owner,
            authority: ctx.accounts.authority.key(),
            withdrawn: true,
            amount,
            owner_support: support.amount,
            total_locked: proposal.total_locked,
            status: proposal.status.code(),
            proposal_tombstoned: proposal.total_locked == 0
        });
        if support.amount == 0 {
            support.close(ctx.accounts.rent_refund.to_account_info())?;
        }
        super::save_released_proposal(
            &proposal,
            &ctx.accounts.proposal.to_account_info(),
            &ctx.accounts.proposer.to_account_info(),
        )
    }
}
