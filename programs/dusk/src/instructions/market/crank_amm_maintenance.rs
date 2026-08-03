use anchor_lang::prelude::*;

use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    state::{FutarchyAuthority, Market},
};

/// Permissionless bounded maintenance for every AMM with an enabled center
/// controller or an active parameter ramp. If hLP
/// supply exists, inventory correction remains a separate one-vault crank so
/// neither instruction combines two bounded-but-expensive workloads.
#[derive(Accounts)]
pub struct CrankAmmMaintenance<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        seeds = [crate::constants::FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    /// Anyone may pay to advance already-authorized, fully funded state.
    pub keeper: Signer<'info>,
}

impl CrankAmmMaintenance<'_> {
    pub fn validate(&self) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(
            self.market.amm.initialized
                && (self.market.config.amm.adjustment_step_nad > 0 || self.market.amm.ramp.active),
            ErrorCode::InvalidArgument
        );
        Ok(())
    }

    pub fn handle_crank(ctx: Context<Self>) -> Result<()> {
        let current_slot = Clock::get()?.slot;
        ctx.accounts.market.accrue_interest_to_slot(current_slot)?;
        ctx.accounts.market.crank_concentrated_amm_with_hlp(current_slot)?;
        Ok(())
    }
}
