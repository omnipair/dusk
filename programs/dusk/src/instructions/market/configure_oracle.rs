use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    state::{Market, OraclePolicy},
};
use anchor_lang::prelude::*;

#[event_cpi]
#[derive(Accounts)]
pub struct ConfigureInitialOracle<'info> {
    #[account(mut, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(),
        market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    pub authority: Signer<'info>,
}

#[event]
pub struct InitialOracleConfigured {
    pub market: Pubkey,
    pub policy: OraclePolicy,
    pub revision: u64,
}

impl ConfigureInitialOracle<'_> {
    pub fn handle(ctx: Context<Self>, policy: OraclePolicy) -> Result<()> {
        let market = &mut ctx.accounts.market;
        market.assert_current_version()?;
        require_eq!(market.base_side.shares.ylp_supply, 0, ErrorCode::NonZeroSupply);
        require_eq!(market.quote_side.shares.ylp_supply, 0, ErrorCode::NonZeroSupply);
        market.require_initial_liquidity_authority(ctx.accounts.authority.key())?;
        policy.validate(market.config.oracle_bounds)?;
        market.oracle.policy = policy;
        market.oracle.last_observation = Default::default();
        market.oracle.runtime = Default::default();
        market.oracle.revision = market
            .oracle
            .revision
            .checked_add(1)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        // Configuration does not trade. First liquidity must authenticate a
        // current report; the creator cannot persist a reusable capability.
        emit_cpi!(InitialOracleConfigured {
            market: market.key(),
            policy,
            revision: market.oracle.revision
        });
        Ok(())
    }
}
