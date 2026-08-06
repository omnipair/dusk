use anchor_lang::prelude::*;

use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    events::ParameterProposalExecuted,
    state::{Market, ParameterProposal, ParameterProposalStatus},
};

use super::current_parameter_revision;

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
