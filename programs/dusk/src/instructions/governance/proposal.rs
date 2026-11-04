use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{
    account::get_size_with_discriminator,
    constants::*,
    errors::ErrorCode,
    events::ParameterProposalExecuted,
    instructions::accounts::validate_lp_mint,
    state::{
        Market, MarketParameterUpdate, ParameterProposal, ParameterProposalStatus, ProposalMetadataV1, ProposalSupport,
        YieldAccount, YieldTokenKind,
    },
    token::{create_token_account, token_burn},
};

use super::{
    carry_forward_governance_yield, checkpoint_supporter_yield, direct_srlp_eligible_supply,
    validate_governance_token_accounts, validate_market_pda, validate_supporter_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateParameterProposalArgs {
    pub nonce: u64,
    /// One to nine updates, at most one per family, in ascending family
    /// order. One sponsorship and one vote cover all of them.
    pub updates: Vec<MarketParameterUpdate>,
    pub action: crate::state::GovernanceAction,
    pub conditions: Vec<crate::state::FamilyRevisionCondition>,
    pub metadata: ProposalMetadataV1,
    pub initial_support: u64,
}

#[event_cpi]
#[derive(Accounts)]
// Account validation only needs the serialized instruction prefix (`nonce`).
// Deserializing the full metadata/update payload here needlessly expands the
// SBF `try_accounts` frame; the handler still receives and validates the full
// `CreateParameterProposalArgs` value.
#[instruction(nonce: u64)]
pub struct CreateParameterProposal<'info> {
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

    #[account(init_if_needed, payer = proposer, space = get_size_with_discriminator::<crate::state::GovernanceControl>(), seeds = [crate::state::GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump)]
    pub governance_control: Box<Account<'info, crate::state::GovernanceControl>>,

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

    #[account(
        init,
        payer = proposer,
        space = get_size_with_discriminator::<ProposalSupport>(),
        seeds = [
            PROPOSAL_SUPPORT_SEED_PREFIX,
            proposal.key().as_ref(),
            proposer.key().as_ref(),
        ],
        bump,
    )]
    pub proposal_support: Box<Account<'info, ProposalSupport>>,

    #[account(mut, address = market.srlp_mint)]
    pub srlp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub proposer_srlp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            proposer.key().as_ref(),
            srlp_mint.key().as_ref(),
            market.base_side.asset_mint.as_ref(),
            &[YieldTokenKind::Srlp.code()],
        ],
        bump = base_yield_account.bump,
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            proposer.key().as_ref(),
            srlp_mint.key().as_ref(),
            market.quote_side.asset_mint.as_ref(),
            &[YieldTokenKind::Srlp.code()],
        ],
        bump = quote_yield_account.bump,
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    /// CHECK: Canonical PDA. A System-owned empty account is initialized as a
    /// Token-2022 srLP vault by the handler; an existing vault is fully parsed
    /// and validated before proposal state changes.
    #[account(
        mut,
        seeds = [
            HLP_SRLP_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            market.base_side.hlp_mint.as_ref(),
            srlp_mint.key().as_ref(),
        ],
        bump,
    )]
    pub base_hlp_srlp_vault: UncheckedAccount<'info>,

    /// CHECK: Validated and initialized under the same rules as the base-side
    /// hLP srLP vault above.
    #[account(
        mut,
        seeds = [
            HLP_SRLP_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            market.quote_side.hlp_mint.as_ref(),
            srlp_mint.key().as_ref(),
        ],
        bump,
    )]
    pub quote_hlp_srlp_vault: UncheckedAccount<'info>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> CreateParameterProposal<'info> {
    pub fn validate(&self, args: &CreateParameterProposalArgs) -> Result<u64> {
        require!(args.initial_support > 0, ErrorCode::AmountZero);
        require_gte!(
            self.proposer_srlp_account.amount,
            args.initial_support,
            ErrorCode::InsufficientBalance
        );
        args.metadata.validate()?;
        super::validate_governance_action(&self.market, &self.governance_control, args.action, &args.updates)?;
        validate_market_pda(&self.market, self.market.key())?;
        require_keys_eq!(self.market.srlp_mint, self.srlp_mint.key(), ErrorCode::InvalidLpMintKey);
        validate_lp_mint(&self.srlp_mint, self.market.key(), self.market.base_side.asset_decimals)?;
        let base_hlp_srlp_amount = governance_vault_amount(
            &self.market,
            &self.srlp_mint,
            &self.base_hlp_srlp_vault,
            self.market.base_hlp_vault.srlp_vault,
            self.market.base_hlp_vault.srlp_shares,
        )?;
        let quote_hlp_srlp_amount = governance_vault_amount(
            &self.market,
            &self.srlp_mint,
            &self.quote_hlp_srlp_vault,
            self.market.quote_hlp_vault.srlp_vault,
            self.market.quote_hlp_vault.srlp_shares,
        )?;
        validate_supporter_accounts(
            &self.market,
            self.proposer.key(),
            &self.srlp_mint,
            &self.proposer_srlp_account,
            &self.base_yield_account,
            &self.quote_yield_account,
        )?;
        direct_srlp_eligible_supply(
            &self.market,
            self.srlp_mint.supply,
            base_hlp_srlp_amount,
            quote_hlp_srlp_amount,
        )
    }

    pub fn handle_create(ctx: Context<'_, '_, '_, 'info, Self>, args: CreateParameterProposalArgs) -> Result<()> {
        ctx.accounts
            .governance_control
            .initialize_if_needed(ctx.accounts.market.key(), ctx.bumps.governance_control)?;
        let eligible_supply = ctx.accounts.validate(&args)?;
        let sponsorship_floor = crate::state::sponsorship_floor(eligible_supply)?;
        require_gte!(
            args.initial_support,
            sponsorship_floor,
            ErrorCode::ProposalSponsorshipTooLow
        );

        let clock = Clock::get()?;
        let market_key = ctx.accounts.market.key();
        let proposer_key = ctx.accounts.proposer.key();
        let proposal_key = ctx.accounts.proposal.key();
        let market_revisions = ctx.accounts.market.parameter_revisions;

        create_token_account(
            &ctx.accounts.market.to_account_info(),
            &ctx.accounts.proposer.to_account_info(),
            &ctx.accounts.base_hlp_srlp_vault.to_account_info(),
            &ctx.accounts.srlp_mint.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.token_2022_program.to_account_info(),
            &[
                HLP_SRLP_VAULT_SEED_PREFIX,
                market_key.as_ref(),
                ctx.accounts.market.base_side.hlp_mint.as_ref(),
                ctx.accounts.srlp_mint.key().as_ref(),
                &[ctx.bumps.base_hlp_srlp_vault],
            ],
        )?;
        create_token_account(
            &ctx.accounts.market.to_account_info(),
            &ctx.accounts.proposer.to_account_info(),
            &ctx.accounts.quote_hlp_srlp_vault.to_account_info(),
            &ctx.accounts.srlp_mint.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.token_2022_program.to_account_info(),
            &[
                HLP_SRLP_VAULT_SEED_PREFIX,
                market_key.as_ref(),
                ctx.accounts.market.quote_side.hlp_mint.as_ref(),
                ctx.accounts.srlp_mint.key().as_ref(),
                &[ctx.bumps.quote_hlp_srlp_vault],
            ],
        )?;
        crate::instructions::accounting::accrue_market_interest(
            &mut ctx.accounts.market,
            clock.slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        let indexes = carry_forward_governance_yield(&mut ctx.accounts.market, clock.slot)?;

        ctx.accounts.proposal.initialize(
            market_key,
            proposer_key,
            args.nonce,
            ctx.accounts.market.tranches.generation,
            &market_revisions,
            args.updates,
            args.action,
            args.conditions,
            args.metadata,
            eligible_supply,
            clock.unix_timestamp,
            ctx.bumps.proposal,
        )?;
        ctx.accounts.proposal_support.initialize(
            proposal_key,
            proposer_key,
            indexes.base_swap_fee_q64,
            indexes.base_interest_q64,
            indexes.quote_swap_fee_q64,
            indexes.quote_interest_q64,
            ctx.bumps.proposal_support,
        );

        checkpoint_supporter_yield(
            &ctx.accounts.market,
            &mut ctx.accounts.base_yield_account,
            &mut ctx.accounts.quote_yield_account,
            ctx.accounts.proposer_srlp_account.amount,
            indexes,
        )?;
        ctx.accounts.proposal_support.accrue_virtual_yield(
            indexes.base_swap_fee_q64,
            indexes.base_interest_q64,
            indexes.quote_swap_fee_q64,
            indexes.quote_interest_q64,
        )?;
        token_burn(
            ctx.accounts.proposer.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.accounts.srlp_mint.to_account_info(),
            ctx.accounts.proposer_srlp_account.to_account_info(),
            args.initial_support,
            &[],
        )?;

        super::adjust_supporter_income_balance(
            &mut ctx.accounts.base_yield_account,
            &mut ctx.accounts.quote_yield_account,
            args.initial_support,
            true,
        )?;
        ctx.accounts.proposal_support.locked_amount = args.initial_support;
        ctx.accounts.proposal.total_locked = args.initial_support;
        ctx.accounts.market.governance_locked_srlp = ctx
            .accounts
            .market
            .governance_locked_srlp
            .checked_add(args.initial_support)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let queued = super::queue_supported_proposal(
            &mut ctx.accounts.proposal,
            proposal_key,
            &mut ctx.accounts.governance_control,
            &ctx.accounts.market,
            eligible_supply,
            clock.unix_timestamp,
        )?;
        emit_cpi!(super::created_event(
            &ctx.accounts.proposal,
            proposal_key,
            ctx.accounts.proposal.current_support(&ctx.accounts.market.tranches)?
        ));
        if queued {
            emit_cpi!(super::queued_event(&ctx.accounts.proposal, ctx.accounts.proposal.key()));
        }
        Ok(())
    }
}

pub(super) fn governance_vault_amount(
    market: &Account<Market>,
    srlp_mint: &InterfaceAccount<Mint>,
    vault: &UncheckedAccount,
    expected_vault: Pubkey,
    tracked_srlp_shares: u64,
) -> Result<u64> {
    require_keys_eq!(vault.key(), expected_vault, ErrorCode::InvalidHlpVault);
    let vault_info = vault.to_account_info();
    if *vault_info.owner == System::id() {
        require!(vault_info.data_is_empty(), ErrorCode::InvalidHlpVault);
        require_eq!(tracked_srlp_shares, 0, ErrorCode::InvalidHlpVault);
        return Ok(0);
    }
    require_keys_eq!(*vault_info.owner, Token2022::id(), ErrorCode::InvalidTokenProgram);
    let data = vault_info.try_borrow_data()?;
    let mut data_slice: &[u8] = &data;
    let account = TokenAccount::try_deserialize_unchecked(&mut data_slice)?;
    require_keys_eq!(account.mint, srlp_mint.key(), ErrorCode::InvalidHlpVault);
    require_keys_eq!(account.owner, market.key(), ErrorCode::InvalidHlpVault);
    require_gte!(account.amount, tracked_srlp_shares, ErrorCode::InvalidHlpVault);
    Ok(account.amount)
}

#[event_cpi]
#[derive(Accounts)]
pub struct QueueParameterProposal<'info> {
    #[account(
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [crate::state::GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump)]
    pub governance_control: Box<Account<'info, crate::state::GovernanceControl>>,

    #[account(mut)]
    pub proposal: Box<Account<'info, ParameterProposal>>,

    #[account(address = market.srlp_mint)]
    pub srlp_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_hlp_srlp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub quote_hlp_srlp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
}

impl<'info> QueueParameterProposal<'info> {
    pub fn validate(&self) -> Result<u64> {
        self.proposal.assert_account(self.market.key(), self.proposal.key())?;
        require!(
            self.proposal.status == ParameterProposalStatus::Collecting,
            ErrorCode::ProposalNotCollecting
        );
        validate_governance_token_accounts(
            &self.market,
            &self.srlp_mint,
            &self.base_hlp_srlp_vault,
            &self.quote_hlp_srlp_vault,
        )?;
        direct_srlp_eligible_supply(
            &self.market,
            self.srlp_mint.supply,
            self.base_hlp_srlp_vault.amount,
            self.quote_hlp_srlp_vault.amount,
        )
    }

    pub fn handle_queue(ctx: Context<Self>) -> Result<()> {
        let eligible_supply = ctx.accounts.validate()?;
        let proposal_key = ctx.accounts.proposal.key();
        let market_revisions = ctx.accounts.market.parameter_revisions;
        if ctx.accounts.proposal.mark_stale_if_revision_changed(&market_revisions) {
            emit_cpi!(super::ProposalResolved {
                proposal: proposal_key,
                market: ctx.accounts.market.key(),
                status: ParameterProposalStatus::Stale,
                resolved_at: Clock::get()?.unix_timestamp
            });
            return Ok(());
        }
        require!(
            super::queue_supported_proposal(
                &mut ctx.accounts.proposal,
                proposal_key,
                &mut ctx.accounts.governance_control,
                &ctx.accounts.market,
                eligible_supply,
                Clock::get()?.unix_timestamp,
            )?,
            ErrorCode::ProposalSupportInsufficient
        );
        emit_cpi!(super::queued_event(&ctx.accounts.proposal, ctx.accounts.proposal.key()));
        Ok(())
    }
}

#[event_cpi]
#[derive(Accounts)]
pub struct ExecuteParameterProposal<'info> {
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

    #[account(mut, seeds = [crate::state::GOVERNANCE_CONTROL_SEED, market.key().as_ref()], bump)]
    pub governance_control: Box<Account<'info, crate::state::GovernanceControl>>,

    #[account(address = market.base_side.asset_mint @ ErrorCode::InvalidMint)]
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address = market.quote_side.asset_mint @ ErrorCode::InvalidMint)]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub proposal: Box<Account<'info, ParameterProposal>>,
    #[account(address = market.srlp_mint)]
    pub srlp_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Exact canonical custody, including authenticated zero custody before hLP initialization.
    pub base_hlp_srlp_vault: UncheckedAccount<'info>,
    /// CHECK: Exact canonical custody, including authenticated zero custody before hLP initialization.
    pub quote_hlp_srlp_vault: UncheckedAccount<'info>,
    /// CHECK: Mandatory canonical address; absent/System-empty or a fully authenticated ballot.
    #[account(mut, seeds = [crate::state::RECOVERY_BALLOT_SEED, market.key().as_ref(), crate::state::RECOVERY_REMOVAL_KEY.as_ref()], bump)]
    pub removal_ballot: UncheckedAccount<'info>,
    /// CHECK: Mandatory canonical address; an executor cannot omit a live objection.
    #[account(mut, seeds = [crate::state::RECOVERY_BALLOT_SEED, market.key().as_ref(), proposal.key().as_ref()], bump)]
    pub cancellation_ballot: UncheckedAccount<'info>,
}

impl<'info> ExecuteParameterProposal<'info> {
    pub fn validate(&self) -> Result<()> {
        self.proposal.assert_account(self.market.key(), self.proposal.key())?;
        require!(
            self.proposal.status == ParameterProposalStatus::Queued,
            ErrorCode::ProposalNotQueued
        );
        Ok(())
    }

    pub fn handle_execute(ctx: Context<Self>) -> Result<ParameterProposalStatus> {
        ctx.accounts.validate()?;
        let clock = Clock::get()?;
        let key = ctx.accounts.proposal.key();
        let revisions = ctx.accounts.market.parameter_revisions;
        let eligible = super::governance_eligible_from_accounts(
            &ctx.accounts.market,
            &ctx.accounts.srlp_mint,
            &ctx.accounts.base_hlp_srlp_vault,
            &ctx.accounts.quote_hlp_srlp_vault,
        )?;
        let epoch = ctx.accounts.governance_control.manager_epoch;
        if let Some(ballot) = super::recovery::refresh_canonical_ballot(
            &ctx.accounts.removal_ballot.to_account_info(),
            None,
            &ctx.accounts.market,
            &mut ctx.accounts.governance_control,
            crate::state::RecoveryTarget::RemoveManager { epoch },
            eligible,
            &clock,
        )? {
            emit_cpi!(super::recovery::ballot_event(
                ctx.accounts.removal_ballot.key(),
                &ballot
            ));
            emit_cpi!(super::ManagerChanged {
                market: ctx.accounts.market.key(),
                manager: Pubkey::default(),
                epoch: ctx.accounts.governance_control.manager_epoch
            });
        }
        let target = crate::state::RecoveryTarget::CancelProposal {
            proposal: key,
            digest: ctx.accounts.proposal.digest,
        };
        if let Some(ballot) = super::recovery::refresh_canonical_ballot(
            &ctx.accounts.cancellation_ballot.to_account_info(),
            Some(ctx.accounts.proposal.to_account_info()),
            &ctx.accounts.market,
            &mut ctx.accounts.governance_control,
            target,
            eligible,
            &clock,
        )? {
            ctx.accounts.proposal.reload()?;
            emit_cpi!(super::recovery::ballot_event(
                ctx.accounts.cancellation_ballot.key(),
                &ballot
            ));
        }
        let effective =
            ctx.accounts
                .proposal
                .effective_status(&ctx.accounts.governance_control, &revisions, key, &clock);
        if effective != ParameterProposalStatus::Queued {
            ctx.accounts.proposal.status = effective;
            ctx.accounts.governance_control.release(key);
            emit_cpi!(super::ProposalResolved {
                proposal: key,
                market: ctx.accounts.market.key(),
                status: effective,
                resolved_at: clock.unix_timestamp
            });
            return Ok(effective);
        }
        require_gte!(
            ctx.accounts.proposal.origin.clock_value(&clock)?,
            ctx.accounts.proposal.execute_after,
            ErrorCode::ProposalTimelockNotReady
        );
        match ctx.accounts.proposal.action {
            crate::state::GovernanceAction::AppointManager {
                manager,
                expected_epoch,
            } => {
                require!(
                    ctx.accounts.proposal.origin == crate::state::ProposalOrigin::Lp,
                    ErrorCode::InvalidManager
                );
                ctx.accounts
                    .governance_control
                    .initialize_baseline(&ctx.accounts.market);
                ctx.accounts
                    .governance_control
                    .replace_manager(manager, expected_epoch)?;
                emit_cpi!(super::ManagerChanged {
                    market: ctx.accounts.market.key(),
                    manager,
                    epoch: ctx.accounts.governance_control.manager_epoch
                });
            }
            crate::state::GovernanceAction::Parameters => {
                let is_manager = matches!(
                    ctx.accounts.proposal.origin,
                    crate::state::ProposalOrigin::Manager { .. }
                );
                ctx.accounts.governance_control.assert_pending(key, &clock)?;
                let updates = super::effective_parameter_updates(&ctx.accounts.market, &ctx.accounts.proposal.updates);
                if !updates.is_empty() {
                    crate::instructions::accounting::accrue_market_interest(
                        &mut ctx.accounts.market,
                        clock.slot,
                        ctx.accounts.event_authority.to_account_info(),
                    )?;
                    ctx.accounts.market.execute_parameter_updates_authenticated(
                        &updates,
                        &clock,
                        ctx.remaining_accounts,
                    )?;
                    if !MarketParameterUpdate::is_oracle_only(&updates) {
                        let fees = crate::instructions::lending_market_admission_fees(
                            &ctx.accounts.market,
                            &ctx.accounts.base_mint,
                            &ctx.accounts.quote_mint,
                            clock.epoch,
                        )?;
                        ctx.accounts.market.assert_market_health_with_fees(fees)?;
                    }
                }
                if !is_manager {
                    for update in &ctx.accounts.proposal.updates {
                        let index = update.family().code() as usize;
                        // An LP ratification can change the manager's reference
                        // even when its exact target already equals the market.
                        // Such an approval must invalidate earlier snapshots too.
                        if ctx.accounts.market.parameter_revisions[index] == revisions[index] {
                            ctx.accounts.market.parameter_revisions[index] =
                                revisions[index].checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
                        }
                        ctx.accounts.governance_control.reanchor(update);
                    }
                }
            }
        }
        ctx.accounts.proposal.status = ParameterProposalStatus::Executed;
        ctx.accounts.governance_control.release(key);
        emit_cpi!(ParameterProposalExecuted {
            proposal: key,
            market: ctx.accounts.market.key(),
            families: ctx.accounts.proposal.families().map(|family| family.code()).collect(),
            new_family_revisions: ctx.accounts.market.parameter_revisions,
            executed_at: clock.unix_timestamp,
        });
        emit_cpi!(super::ProposalResolved {
            proposal: key,
            market: ctx.accounts.market.key(),
            status: ParameterProposalStatus::Executed,
            resolved_at: clock.unix_timestamp
        });
        Ok(ParameterProposalStatus::Executed)
    }
}
