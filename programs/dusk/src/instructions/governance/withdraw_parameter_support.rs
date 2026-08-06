use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::ParameterProposalSupportWithdrawn,
    generate_market_seeds,
    instructions::common::validate_lp_mint,
    shared::token::token_mint_to,
    state::{Market, ParameterProposal, ParameterProposalStatus, ProposalSupport, YieldAccount, YieldTokenKind},
};

use super::{
    carry_forward_governance_yield, checkpoint_supporter_yield, current_parameter_revision, validate_market_pda,
    validate_supporter_accounts,
};

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
