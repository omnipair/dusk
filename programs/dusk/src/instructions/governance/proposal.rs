use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{ParameterProposalCreated, ParameterProposalExecuted, ParameterProposalQueued},
    instructions::common::validate_lp_mint,
    shared::{
        account::get_size_with_discriminator,
        token::{create_token_account, token_burn},
    },
    state::{
        Market, MarketParameterUpdate, ParameterProposal, ParameterProposalStatus, ProposalMetadataV1, ProposalSupport,
        YieldAccount, YieldTokenKind,
    },
};

use super::{
    carry_forward_governance_yield, checkpoint_supporter_yield, current_parameter_revision, direct_ylp_eligible_supply,
    validate_governance_token_accounts, validate_market_pda, validate_supporter_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateParameterProposalArgs {
    pub nonce: u64,
    pub update: MarketParameterUpdate,
    pub metadata: ProposalMetadataV1,
    pub initial_support: u64,
}

#[derive(Accounts)]
#[instruction(args: CreateParameterProposalArgs)]
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

    #[account(
        init,
        payer = proposer,
        space = get_size_with_discriminator::<ParameterProposal>(),
        seeds = [
            PARAMETER_PROPOSAL_SEED_PREFIX,
            market.key().as_ref(),
            proposer.key().as_ref(),
            args.nonce.to_le_bytes().as_ref(),
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

    #[account(mut, address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub proposer_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            proposer.key().as_ref(),
            ylp_mint.key().as_ref(),
            market.base_side.asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
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
            ylp_mint.key().as_ref(),
            market.quote_side.asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump = quote_yield_account.bump,
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

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

impl<'info> CreateParameterProposal<'info> {
    pub fn validate(&self, args: &CreateParameterProposalArgs) -> Result<u64> {
        require!(args.initial_support > 0, ErrorCode::AmountZero);
        require_gte!(
            self.proposer_ylp_account.amount,
            args.initial_support,
            ErrorCode::InsufficientBalance
        );
        args.metadata.validate()?;
        self.market.validate_parameter_update(&args.update)?;
        validate_market_pda(&self.market, self.market.key())?;
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.market.base_side.asset_decimals)?;
        let base_hlp_ylp_amount = governance_vault_amount(
            &self.market,
            &self.ylp_mint,
            &self.base_hlp_ylp_vault,
            self.market.base_hlp_vault.ylp_vault,
            self.market.base_hlp_vault.ylp_shares,
        )?;
        let quote_hlp_ylp_amount = governance_vault_amount(
            &self.market,
            &self.ylp_mint,
            &self.quote_hlp_ylp_vault,
            self.market.quote_hlp_vault.ylp_vault,
            self.market.quote_hlp_vault.ylp_shares,
        )?;
        validate_supporter_accounts(
            &self.market,
            self.proposer.key(),
            &self.ylp_mint,
            &self.proposer_ylp_account,
            &self.base_yield_account,
            &self.quote_yield_account,
        )?;
        direct_ylp_eligible_supply(
            &self.market,
            self.ylp_mint.supply,
            base_hlp_ylp_amount,
            quote_hlp_ylp_amount,
        )
    }

    pub fn handle_create(ctx: Context<'_, '_, '_, 'info, Self>, args: CreateParameterProposalArgs) -> Result<()> {
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
        let family_revision = current_parameter_revision(&ctx.accounts.market, args.update.family());

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
        let indexes = carry_forward_governance_yield(&mut ctx.accounts.market, clock.slot)?;

        ctx.accounts.proposal.initialize(
            market_key,
            proposer_key,
            args.nonce,
            family_revision,
            args.update,
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
            &mut ctx.accounts.base_yield_account,
            &mut ctx.accounts.quote_yield_account,
            ctx.accounts.proposer_ylp_account.amount,
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
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.proposer_ylp_account.to_account_info(),
            args.initial_support,
            &[],
        )?;

        ctx.accounts.proposal_support.locked_amount = args.initial_support;
        ctx.accounts.proposal.total_locked = args.initial_support;
        ctx.accounts.market.governance_locked_ylp = ctx
            .accounts
            .market
            .governance_locked_ylp
            .checked_add(args.initial_support)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let queued = ctx
            .accounts
            .proposal
            .queue_if_supported(eligible_supply, clock.unix_timestamp)?;
        emit!(ParameterProposalCreated {
            proposal: proposal_key,
            market: market_key,
            proposer: proposer_key,
            nonce: ctx.accounts.proposal.nonce,
            family: ctx.accounts.proposal.family.code(),
            family_revision: ctx.accounts.proposal.family_revision,
            digest: ctx.accounts.proposal.digest,
            sponsorship_floor: ctx.accounts.proposal.sponsorship_floor,
            initial_support: args.initial_support,
            status: ctx.accounts.proposal.status.code(),
        });
        if queued {
            emit!(ParameterProposalQueued {
                proposal: proposal_key,
                total_locked: ctx.accounts.proposal.queued_support,
                eligible_supply: ctx.accounts.proposal.queued_eligible_ylp,
                queued_at: ctx.accounts.proposal.queued_at,
                execute_after: ctx.accounts.proposal.execute_after,
                execution_deadline: ctx.accounts.proposal.execution_deadline,
            });
        }
        Ok(())
    }
}

fn governance_vault_amount(
    market: &Account<Market>,
    ylp_mint: &InterfaceAccount<Mint>,
    vault: &UncheckedAccount,
    expected_vault: Pubkey,
    tracked_ylp_shares: u64,
) -> Result<u64> {
    require_keys_eq!(vault.key(), expected_vault, ErrorCode::InvalidHlpVault);
    let vault_info = vault.to_account_info();
    if *vault_info.owner == System::id() {
        require!(vault_info.data_is_empty(), ErrorCode::InvalidHlpVault);
        require_eq!(tracked_ylp_shares, 0, ErrorCode::InvalidHlpVault);
        return Ok(0);
    }
    require_keys_eq!(*vault_info.owner, Token2022::id(), ErrorCode::InvalidTokenProgram);
    let data = vault_info.try_borrow_data()?;
    let mut data_slice: &[u8] = &data;
    let account = TokenAccount::try_deserialize_unchecked(&mut data_slice)?;
    require_keys_eq!(account.mint, ylp_mint.key(), ErrorCode::InvalidHlpVault);
    require_keys_eq!(account.owner, market.key(), ErrorCode::InvalidHlpVault);
    require_gte!(account.amount, tracked_ylp_shares, ErrorCode::InvalidHlpVault);
    Ok(account.amount)
}

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

    #[account(mut)]
    pub proposal: Box<Account<'info, ParameterProposal>>,

    #[account(address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub quote_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
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
            &self.ylp_mint,
            &self.base_hlp_ylp_vault,
            &self.quote_hlp_ylp_vault,
        )?;
        direct_ylp_eligible_supply(
            &self.market,
            self.ylp_mint.supply,
            self.base_hlp_ylp_vault.amount,
            self.quote_hlp_ylp_vault.amount,
        )
    }

    pub fn handle_queue(ctx: Context<Self>) -> Result<()> {
        let eligible_supply = ctx.accounts.validate()?;
        let current_revision = current_parameter_revision(&ctx.accounts.market, ctx.accounts.proposal.family);
        if ctx.accounts.proposal.mark_stale_if_revision_changed(current_revision) {
            return Ok(());
        }
        require!(
            ctx.accounts
                .proposal
                .queue_if_supported(eligible_supply, Clock::get()?.unix_timestamp)?,
            ErrorCode::ProposalSupportInsufficient
        );
        emit!(ParameterProposalQueued {
            proposal: ctx.accounts.proposal.key(),
            total_locked: ctx.accounts.proposal.queued_support,
            eligible_supply: ctx.accounts.proposal.queued_eligible_ylp,
            queued_at: ctx.accounts.proposal.queued_at,
            execute_after: ctx.accounts.proposal.execute_after,
            execution_deadline: ctx.accounts.proposal.execution_deadline,
        });
        Ok(())
    }
}

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

    #[account(mut)]
    pub proposal: Box<Account<'info, ParameterProposal>>,
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

    pub fn handle_execute(ctx: Context<Self>) -> Result<()> {
        ctx.accounts.validate()?;
        let current_revision = current_parameter_revision(&ctx.accounts.market, ctx.accounts.proposal.family);
        if ctx.accounts.proposal.mark_stale_if_revision_changed(current_revision) {
            return Ok(());
        }

        let clock = Clock::get()?;
        require_gte!(
            clock.unix_timestamp,
            ctx.accounts.proposal.execute_after,
            ErrorCode::ProposalTimelockNotReady
        );
        require_gte!(
            ctx.accounts.proposal.execution_deadline,
            clock.unix_timestamp,
            ErrorCode::ProposalExecutionWindowExpired
        );
        ctx.accounts
            .market
            .execute_parameter_update(&ctx.accounts.proposal.update, clock.slot)?;
        ctx.accounts.proposal.status = ParameterProposalStatus::Executed;
        emit!(ParameterProposalExecuted {
            proposal: ctx.accounts.proposal.key(),
            market: ctx.accounts.market.key(),
            family: ctx.accounts.proposal.family.code(),
            new_family_revision: current_parameter_revision(&ctx.accounts.market, ctx.accounts.proposal.family,),
            executed_at: clock.unix_timestamp,
        });
        Ok(())
    }
}
