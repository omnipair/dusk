use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{
    constants::{HLP_COHORT_SEED_PREFIX, MARKET_V2_SEED_PREFIX, YIELD_ACCOUNT_SEED_PREFIX},
    errors::ErrorCode,
    events::HlpRetiredReceiptBurned,
    instructions::accounts::{validate_lp_mint, validate_owner_lp_account},
    state::{HlpCohort, Market, YieldAccount, YieldTokenKind},
    token::token_burn,
    transitions::liquidity::drain_hlp_vault_unallocated_yield,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BurnRetiredHlpReceiptArgs {
    pub amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: BurnRetiredHlpReceiptArgs)]
pub struct BurnRetiredHlpReceipt<'info> {
    #[account(
        seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut)]
    pub retired_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [HLP_COHORT_SEED_PREFIX, market.key().as_ref(), retired_mint.key().as_ref()],
        bump = cohort.bump
    )]
    pub cohort: Box<Account<'info, HlpCohort>>,
    #[account(mut)]
    pub owner_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), retired_mint.key().as_ref(), market.base_side.asset_mint.as_ref(), &[YieldTokenKind::Hlp.code()]],
        bump = base_yield_account.bump
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(
        mut,
        seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), retired_mint.key().as_ref(), market.quote_side.asset_mint.as_ref(), &[YieldTokenKind::Hlp.code()]],
        bump = quote_yield_account.bump
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> BurnRetiredHlpReceipt<'info> {
    pub fn handle_burn(ctx: Context<Self>, args: BurnRetiredHlpReceiptArgs) -> Result<()> {
        require!(args.amount > 0, ErrorCode::AmountZero);
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let mint_key = ctx.accounts.retired_mint.key();
        let target_asset = ctx
            .accounts
            .cohort
            .assert_retired(market_key, mint_key, ctx.accounts.cohort.key())?;
        require_keys_neq!(
            ctx.accounts.market.side(target_asset).hlp_mint,
            mint_key,
            ErrorCode::InvalidLpMintKey
        );
        validate_lp_mint(
            &ctx.accounts.retired_mint,
            market_key,
            ctx.accounts.market.side(target_asset).asset_decimals,
        )?;
        validate_owner_lp_account(owner_key, &ctx.accounts.retired_mint, &ctx.accounts.owner_hlp_account)?;
        ctx.accounts.base_yield_account.assert_account(
            owner_key,
            market_key,
            mint_key,
            ctx.accounts.market.base_side.asset_mint,
            YieldTokenKind::Hlp,
        )?;
        ctx.accounts.quote_yield_account.assert_account(
            owner_key,
            market_key,
            mint_key,
            ctx.accounts.market.quote_side.asset_mint,
            YieldTokenKind::Hlp,
        )?;
        let live_supply = ctx.accounts.retired_mint.supply;
        let vault = &mut ctx.accounts.cohort.final_vault;
        require_gte!(vault.hlp_supply, live_supply, ErrorCode::InvalidHlpMintSupply);
        require!(live_supply > 0, ErrorCode::InvalidHlpMintSupply);
        vault.hlp_supply = live_supply;
        require_gte!(
            ctx.accounts.owner_hlp_account.amount,
            args.amount,
            ErrorCode::InsufficientBalance
        );
        let holder_balance = ctx.accounts.owner_hlp_account.amount;
        ctx.accounts.base_yield_account.accrue(
            holder_balance,
            vault.base_swap_fee_growth_index_q64,
            vault.base_interest_growth_index_q64,
        )?;
        ctx.accounts.quote_yield_account.accrue(
            holder_balance,
            vault.quote_swap_fee_growth_index_q64,
            vault.quote_interest_growth_index_q64,
        )?;
        token_burn(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.accounts.retired_mint.to_account_info(),
            ctx.accounts.owner_hlp_account.to_account_info(),
            args.amount,
            &[],
        )?;
        vault.hlp_supply = vault
            .hlp_supply
            .checked_sub(args.amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if vault.hlp_supply == 0 {
            drain_hlp_vault_unallocated_yield(
                vault,
                &mut ctx.accounts.base_yield_account,
                &mut ctx.accounts.quote_yield_account,
            )?;
        }
        emit_cpi!(HlpRetiredReceiptBurned {
            market: market_key,
            owner: owner_key,
            mint: mint_key,
            amount: args.amount,
            remaining_supply: vault.hlp_supply,
        });
        Ok(())
    }
}
