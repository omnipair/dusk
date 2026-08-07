use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{ParameterProposalQueued, ParameterProposalSupportWithdrawn, ParameterProposalSupported},
    generate_market_seeds,
    instructions::common::validate_lp_mint,
    shared::{
        account::get_size_with_discriminator,
        token::{token_burn, token_mint_to},
    },
    state::{Market, ParameterProposal, ParameterProposalStatus, ProposalSupport, YieldAccount, YieldTokenKind},
};

use super::{
    carry_forward_governance_yield, checkpoint_supporter_yield, current_parameter_revision, direct_ylp_eligible_supply,
    validate_governance_token_accounts, validate_market_pda, validate_supporter_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SupportParameterProposalArgs {
    pub amount: u64,
}

#[derive(Accounts)]
pub struct SupportParameterProposal<'info> {
    #[account(mut)]
    pub supporter: Signer<'info>,

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

    #[account(
        init_if_needed,
        payer = supporter,
        space = get_size_with_discriminator::<ProposalSupport>(),
        seeds = [
            PROPOSAL_SUPPORT_SEED_PREFIX,
            proposal.key().as_ref(),
            supporter.key().as_ref(),
        ],
        bump,
    )]
    pub proposal_support: Box<Account<'info, ProposalSupport>>,

    #[account(mut, address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub supporter_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            supporter.key().as_ref(),
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
            supporter.key().as_ref(),
            ylp_mint.key().as_ref(),
            market.quote_side.asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump = quote_yield_account.bump,
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub base_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub quote_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> SupportParameterProposal<'info> {
    pub fn validate(&self, args: &SupportParameterProposalArgs) -> Result<u64> {
        require!(args.amount > 0, ErrorCode::AmountZero);
        require_gte!(
            self.supporter_ylp_account.amount,
            args.amount,
            ErrorCode::InsufficientBalance
        );
        self.proposal.assert_account(self.market.key(), self.proposal.key())?;
        require!(
            self.proposal.status == ParameterProposalStatus::Collecting,
            ErrorCode::ProposalNotCollecting
        );
        require_eq!(
            current_parameter_revision(&self.market, self.proposal.family),
            self.proposal.family_revision,
            ErrorCode::ProposalStale
        );
        validate_governance_token_accounts(
            &self.market,
            &self.ylp_mint,
            &self.base_hlp_ylp_vault,
            &self.quote_hlp_ylp_vault,
        )?;
        validate_supporter_accounts(
            &self.market,
            self.supporter.key(),
            &self.ylp_mint,
            &self.supporter_ylp_account,
            &self.base_yield_account,
            &self.quote_yield_account,
        )?;
        direct_ylp_eligible_supply(
            &self.market,
            self.ylp_mint.supply,
            self.base_hlp_ylp_vault.amount,
            self.quote_hlp_ylp_vault.amount,
        )
    }

    pub fn handle_support(ctx: Context<'_, '_, '_, 'info, Self>, args: SupportParameterProposalArgs) -> Result<()> {
        let eligible_supply = ctx.accounts.validate(&args)?;
        let clock = Clock::get()?;
        let proposal_key = ctx.accounts.proposal.key();
        let supporter_key = ctx.accounts.supporter.key();
        let indexes = carry_forward_governance_yield(&mut ctx.accounts.market, clock.slot)?;

        if ctx.accounts.proposal_support.proposal == Pubkey::default() {
            ctx.accounts.proposal_support.initialize(
                proposal_key,
                supporter_key,
                indexes.base_swap_fee_q64,
                indexes.base_interest_q64,
                indexes.quote_swap_fee_q64,
                indexes.quote_interest_q64,
                ctx.bumps.proposal_support,
            );
        }
        ctx.accounts
            .proposal_support
            .assert_account(proposal_key, supporter_key, ctx.bumps.proposal_support)?;

        checkpoint_supporter_yield(
            &mut ctx.accounts.base_yield_account,
            &mut ctx.accounts.quote_yield_account,
            ctx.accounts.supporter_ylp_account.amount,
            indexes,
        )?;
        ctx.accounts.proposal_support.accrue_virtual_yield(
            indexes.base_swap_fee_q64,
            indexes.base_interest_q64,
            indexes.quote_swap_fee_q64,
            indexes.quote_interest_q64,
        )?;
        token_burn(
            ctx.accounts.supporter.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.supporter_ylp_account.to_account_info(),
            args.amount,
            &[],
        )?;

        ctx.accounts.proposal_support.locked_amount = ctx
            .accounts
            .proposal_support
            .locked_amount
            .checked_add(args.amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        ctx.accounts.proposal.total_locked = ctx
            .accounts
            .proposal
            .total_locked
            .checked_add(args.amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        ctx.accounts.market.governance_locked_ylp = ctx
            .accounts
            .market
            .governance_locked_ylp
            .checked_add(args.amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let queued = ctx
            .accounts
            .proposal
            .queue_if_supported(eligible_supply, clock.unix_timestamp)?;
        emit!(ParameterProposalSupported {
            proposal: proposal_key,
            supporter: supporter_key,
            amount: args.amount,
            supporter_locked: ctx.accounts.proposal_support.locked_amount,
            total_locked: ctx.accounts.proposal.total_locked,
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

#[derive(Accounts)]
pub struct WithdrawParameterSupport<'info> {
    #[account(mut)]
    pub supporter: Signer<'info>,

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

    #[account(
        mut,
        close = supporter,
        seeds = [
            PROPOSAL_SUPPORT_SEED_PREFIX,
            proposal.key().as_ref(),
            supporter.key().as_ref(),
        ],
        bump = proposal_support.bump,
    )]
    pub proposal_support: Box<Account<'info, ProposalSupport>>,

    #[account(mut, address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub supporter_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            supporter.key().as_ref(),
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
            supporter.key().as_ref(),
            ylp_mint.key().as_ref(),
            market.quote_side.asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump = quote_yield_account.bump,
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> WithdrawParameterSupport<'info> {
    pub fn validate(&self) -> Result<()> {
        validate_market_pda(&self.market, self.market.key())?;
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.market.base_side.asset_decimals)?;
        self.proposal.assert_account(self.market.key(), self.proposal.key())?;
        self.proposal_support
            .assert_account(self.proposal.key(), self.supporter.key(), self.proposal_support.bump)?;
        require!(
            self.proposal_support.locked_amount > 0,
            ErrorCode::InvalidProposalSupport
        );
        validate_supporter_accounts(
            &self.market,
            self.supporter.key(),
            &self.ylp_mint,
            &self.supporter_ylp_account,
            &self.base_yield_account,
            &self.quote_yield_account,
        )
    }

    pub fn handle_withdraw(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        ctx.accounts.validate()?;
        let clock = Clock::get()?;
        let current_revision = current_parameter_revision(&ctx.accounts.market, ctx.accounts.proposal.family);
        ctx.accounts.proposal.mark_stale_if_revision_changed(current_revision);
        ctx.accounts
            .proposal
            .mark_expired_if_past_deadline(clock.unix_timestamp);
        require!(
            ctx.accounts.proposal.status != ParameterProposalStatus::Queued,
            ErrorCode::ProposalSupportFrozen
        );

        let amount = ctx.accounts.proposal_support.locked_amount;
        let indexes = carry_forward_governance_yield(&mut ctx.accounts.market, clock.slot)?;
        checkpoint_supporter_yield(
            &mut ctx.accounts.base_yield_account,
            &mut ctx.accounts.quote_yield_account,
            ctx.accounts.supporter_ylp_account.amount,
            indexes,
        )?;
        ctx.accounts.proposal_support.accrue_virtual_yield(
            indexes.base_swap_fee_q64,
            indexes.base_interest_q64,
            indexes.quote_swap_fee_q64,
            indexes.quote_interest_q64,
        )?;
        ctx.accounts
            .proposal_support
            .base_yield
            .merge_into(&mut ctx.accounts.base_yield_account)?;
        ctx.accounts
            .proposal_support
            .quote_yield
            .merge_into(&mut ctx.accounts.quote_yield_account)?;

        ctx.accounts.proposal.total_locked = ctx
            .accounts
            .proposal
            .total_locked
            .checked_sub(amount)
            .ok_or(ErrorCode::InvalidProposalSupport)?;
        ctx.accounts.market.governance_locked_ylp = ctx
            .accounts
            .market
            .governance_locked_ylp
            .checked_sub(amount)
            .ok_or(ErrorCode::InvalidProposalSupport)?;
        ctx.accounts.proposal.cancel_if_below_sponsorship_floor();

        let market_seeds = generate_market_seeds!(ctx.accounts.market);
        token_mint_to(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.supporter_ylp_account.to_account_info(),
            amount,
            &[&market_seeds[..]],
        )?;
        emit!(ParameterProposalSupportWithdrawn {
            proposal: ctx.accounts.proposal.key(),
            supporter: ctx.accounts.supporter.key(),
            amount,
            total_locked: ctx.accounts.proposal.total_locked,
            status: ctx.accounts.proposal.status.code(),
        });
        Ok(())
    }
}
