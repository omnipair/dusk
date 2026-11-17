use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    instructions::accounts::{require_reserve_custody, validate_swap_fee_custody_accounts},
    state::Market,
};
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

/// Move the permanent seed's credited interest that reserve cash can pay into
/// swap-fee custody. The atoms stay in the reserve vault. This is not
/// principal until balanced reinvestment mints funded reserve units.
#[event_cpi]
#[derive(Accounts)]
pub struct FundJuniorSeed<'info> {
    #[account(mut, constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

#[event]
pub struct JuniorSeedFunded {
    pub market: Pubkey,
    pub asset: Pubkey,
    pub interest_debit: u64,
    pub reserve_credit: u64,
}

impl<'info> FundJuniorSeed<'info> {
    pub fn handle_fund(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let a = &mut *ctx.accounts;
        a.market.assert_current_version()?;
        let asset = validate_swap_fee_custody_accounts(&a.market, &a.asset_mint, &a.reserve_vault)?;
        let amount = a.market.fund_junior_seed(asset)?;
        require!(amount > 0, ErrorCode::AmountZero);
        require_reserve_custody(a.reserve_vault.amount, a.market.side(asset))?;
        let event = JuniorSeedFunded {
            market: a.market.key(),
            asset: a.asset_mint.key(),
            interest_debit: amount,
            reserve_credit: amount,
        };
        emit_cpi!(event);
        Ok(())
    }
}

#[event_cpi]
#[derive(Accounts)]
pub struct CompoundJuniorSeed<'info> {
    #[account(mut, constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(seeds=[crate::constants::FUTARCHY_AUTHORITY_SEED_PREFIX],bump=futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, crate::state::FutarchyAuthority>>,

    #[account(address = market.junior_mint)]
    pub junior_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[event]
pub struct JuniorSeedCompounded {
    pub market: Pubkey,
    pub base_amount: u64,
    pub quote_amount: u64,
    pub reserve_shares: u64,
}

impl<'info> CompoundJuniorSeed<'info> {
    pub fn handle_compound(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let market = &mut ctx.accounts.market;
        market.assert_current_version()?;
        crate::instructions::accounting::accrue_market_interest(
            market,
            Clock::get()?.slot,
            ctx.accounts.event_authority.to_account_info(),
            &ctx.accounts.futarchy_authority,
        )?;
        market.reconcile_junior_receipts(ctx.accounts.junior_mint.supply)?;
        // Swap-fee allocation carries forward whenever srLP supply is
        // positive, so no unallocated fee needs flushing before compounding.
        let receipt = market.compound_junior_seed()?;
        market.finalize_amm_transition_and_observe_risk(Clock::get()?.slot)?;
        market.assert_market_invariants()?;
        emit_cpi!(JuniorSeedCompounded {
            market: market.key(),
            base_amount: receipt.base_reserve_credit,
            quote_amount: receipt.quote_reserve_credit,
            reserve_shares: receipt.srlp_amount
        });
        Ok(())
    }
}
