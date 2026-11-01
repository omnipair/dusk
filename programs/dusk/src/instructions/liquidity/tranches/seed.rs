use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    generate_market_seeds,
    instructions::accounts::{
        require_reserve_custody, token_account_credit, token_program_for_mint, validate_interest_accounts,
        validate_swap_fee_custody_accounts,
    },
    state::Market,
    token::transfer_checked_with_remaining_accounts,
};
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

/// Move only the permanent seed's collected interest into reserve fee custody.
/// This is not principal until balanced reinvestment mints funded reserve units.
#[event_cpi]
#[derive(Accounts)]
pub struct FundJuniorSeed<'info> {
    #[account(mut, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
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
        let asset = validate_interest_accounts(&a.market, &a.asset_mint, &a.interest_vault)?;
        require!(
            asset == validate_swap_fee_custody_accounts(&a.market, &a.asset_mint, &a.reserve_vault)?,
            ErrorCode::InvalidVault
        );
        a.market.checkpoint_junior_seed()?;
        let amount = a.market.side(asset).fees.junior.seed_interest_amount;
        require!(amount > 0, ErrorCode::AmountZero);
        let before = a.reserve_vault.amount;
        transfer_checked_with_remaining_accounts(
            a.market.to_account_info(),
            a.interest_vault.to_account_info(),
            a.reserve_vault.to_account_info(),
            a.asset_mint.to_account_info(),
            token_program_for_mint(&a.asset_mint, &a.token_program, &a.token_2022_program)?,
            amount,
            a.asset_mint.decimals,
            &[&generate_market_seeds!(a.market)[..]],
            ctx.remaining_accounts,
        )?;
        a.reserve_vault.reload()?;
        let credit = token_account_credit(before, &a.reserve_vault)?;
        a.market.fund_junior_seed(asset, amount, credit)?;
        require_reserve_custody(a.reserve_vault.amount, a.market.side(asset))?;
        let event = JuniorSeedFunded {
            market: a.market.key(),
            asset: a.asset_mint.key(),
            interest_debit: amount,
            reserve_credit: credit,
        };
        emit_cpi!(event);
        Ok(())
    }
}

#[event_cpi]
#[derive(Accounts)]
pub struct CompoundJuniorSeed<'info> {
    #[account(mut, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
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

impl CompoundJuniorSeed<'_> {
    pub fn handle_compound(ctx: Context<Self>) -> Result<()> {
        let market = &mut ctx.accounts.market;
        market.assert_current_version()?;
        crate::instructions::accounting::accrue_market_interest(
            market,
            Clock::get()?.slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        market.reconcile_junior_receipts(ctx.accounts.junior_mint.supply)?;
        let receipt = market.compound_junior_seed()?;
        market.finalize_amm_transition_and_observe_risk(Clock::get()?.slot)?;
        market.assert_market_invariants()?;
        emit_cpi!(JuniorSeedCompounded {
            market: market.key(),
            base_amount: receipt.base_reserve_credit,
            quote_amount: receipt.quote_reserve_credit,
            reserve_shares: receipt.ylp_amount
        });
        Ok(())
    }
}
