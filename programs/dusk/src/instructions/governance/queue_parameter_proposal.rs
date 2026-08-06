use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::ParameterProposalQueued,
    state::{Market, ParameterProposal, ParameterProposalStatus},
};

use super::{current_parameter_revision, direct_ylp_eligible_supply, validate_governance_token_accounts};

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
