use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LiquidityRemoved, MarketEventMetadata, MarketHealthUpdated},
    generate_market_seeds,
    shared::token::{token_burn, transfer_checked_with_remaining_accounts},
    state::{Market, YieldAccount, YieldTokenKind},
};

use super::{validate_ylp_market_pda, ylp_yield_account_pda};

use crate::instructions::common::{
    require_reserve_custody, require_supported_asset_mint, token_account_credit, token_account_debit,
    token_program_for_mint, validate_lp_mint, validate_owner_asset_account, validate_owner_lp_account,
    validate_side_vault_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct RemoveLiquidityArgs {
    pub ylp_amount: u64,
    pub min_base_amount_out: u64,
    pub min_quote_amount_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct RemoveLiquidity<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_base_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_quote_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> RemoveLiquidity<'info> {
    pub fn validate(&self, args: &RemoveLiquidityArgs) -> Result<()> {
        validate_ylp_market_pda(&self.market, self.market.key())?;
        self.market.assert_started()?;
        require!(args.ylp_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            self.owner_ylp_account.amount,
            args.ylp_amount,
            ErrorCode::InsufficientBalance
        );
        validate_side_vault_accounts(
            &self.market,
            crate::state::MarketAsset::Base,
            &self.base_mint,
            &self.base_reserve_vault,
        )?;
        validate_side_vault_accounts(
            &self.market,
            crate::state::MarketAsset::Quote,
            &self.quote_mint,
            &self.quote_reserve_vault,
        )?;
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        validate_owner_asset_account(self.owner.key(), &self.base_mint, &self.owner_base_account)?;
        validate_owner_asset_account(self.owner.key(), &self.quote_mint, &self.owner_quote_account)?;
        validate_owner_lp_account(self.owner.key(), &self.ylp_mint, &self.owner_ylp_account)?;
        require_supported_asset_mint(&self.base_mint)?;
        require_supported_asset_mint(&self.quote_mint)?;
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.base_mint.decimals)?;
        let (expected_base_yield_account, expected_base_yield_bump) = ylp_yield_account_pda(
            self.market.key(),
            self.owner.key(),
            self.ylp_mint.key(),
            self.base_mint.key(),
        )?;
        require_keys_eq!(
            self.base_yield_account.key(),
            expected_base_yield_account,
            ErrorCode::InvalidYieldAccount
        );
        require_eq!(
            self.base_yield_account.bump,
            expected_base_yield_bump,
            ErrorCode::InvalidYieldAccount
        );
        self.base_yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.ylp_mint.key(),
            self.base_mint.key(),
            YieldTokenKind::Ylp,
        )?;
        let (expected_quote_yield_account, expected_quote_yield_bump) = ylp_yield_account_pda(
            self.market.key(),
            self.owner.key(),
            self.ylp_mint.key(),
            self.quote_mint.key(),
        )?;
        require_keys_eq!(
            self.quote_yield_account.key(),
            expected_quote_yield_account,
            ErrorCode::InvalidYieldAccount
        );
        require_eq!(
            self.quote_yield_account.bump,
            expected_quote_yield_bump,
            ErrorCode::InvalidYieldAccount
        );
        self.quote_yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.ylp_mint.key(),
            self.quote_mint.key(),
            YieldTokenKind::Ylp,
        )?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(RemoveLiquidityArgs);

    pub fn handle_remove_liquidity(ctx: Context<'_, '_, '_, 'info, Self>, args: RemoveLiquidityArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();

        // Checkpoint the owner's yield before reducing live yLP supply.
        {
            let market = &mut ctx.accounts.market;
            market.base_side.carry_forward_swap_fees()?;
            market.base_side.carry_forward_interest()?;
            market.quote_side.carry_forward_swap_fees()?;
            market.quote_side.carry_forward_interest()?;
            ctx.accounts.base_yield_account.accrue(
                ctx.accounts.owner_ylp_account.amount,
                market.base_side.fees.swap_fee_growth_index_q64,
                market.base_side.fees.interest_growth_index_q64,
            )?;
            ctx.accounts.quote_yield_account.accrue(
                ctx.accounts.owner_ylp_account.amount,
                market.quote_side.fees.swap_fee_growth_index_q64,
                market.quote_side.fees.interest_growth_index_q64,
            )?;
        }

        // Burn yLP before applying the matching reserve withdrawal.
        let ylp_program = token_program_for_mint(
            &ctx.accounts.ylp_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        token_burn(
            ctx.accounts.owner.to_account_info(),
            ylp_program,
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.owner_ylp_account.to_account_info(),
            args.ylp_amount,
            &[],
        )?;

        // Commit the reserve withdrawal and refresh solvency before payout.
        let receipt = ctx.accounts.market.remove_liquidity(args.ylp_amount)?;
        let current_slot = Clock::get()?.slot;
        ctx.accounts
            .market
            .finalize_amm_transition_and_observe_risk(current_slot)?;
        ctx.accounts.market.assert_market_health()?;

        // Transfer both reserve outputs and measure the owner's actual credits.
        let base_reserve_balance_before = ctx.accounts.base_reserve_vault.amount;
        let quote_reserve_balance_before = ctx.accounts.quote_reserve_vault.amount;
        let base_balance_before = ctx.accounts.owner_base_account.amount;
        let quote_balance_before = ctx.accounts.owner_quote_account.amount;
        let base_token_program = token_program_for_mint(
            &ctx.accounts.base_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let quote_token_program = token_program_for_mint(
            &ctx.accounts.quote_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.base_reserve_vault.to_account_info(),
            ctx.accounts.owner_base_account.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            base_token_program,
            receipt.base_amount_out,
            ctx.accounts.base_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            ctx.remaining_accounts,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.quote_reserve_vault.to_account_info(),
            ctx.accounts.owner_quote_account.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            quote_token_program,
            receipt.quote_amount_out,
            ctx.accounts.quote_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.base_reserve_vault.reload()?;
        ctx.accounts.quote_reserve_vault.reload()?;
        ctx.accounts.owner_base_account.reload()?;
        ctx.accounts.owner_quote_account.reload()?;
        require_reserve_custody(ctx.accounts.base_reserve_vault.amount, &ctx.accounts.market.base_side)?;
        require_reserve_custody(ctx.accounts.quote_reserve_vault.amount, &ctx.accounts.market.quote_side)?;
        let base_reserve_debit = token_account_debit(base_reserve_balance_before, &ctx.accounts.base_reserve_vault)?;
        let quote_reserve_debit = token_account_debit(quote_reserve_balance_before, &ctx.accounts.quote_reserve_vault)?;
        require_eq!(
            base_reserve_debit,
            receipt.base_amount_out,
            ErrorCode::MarketMathOverflow
        );
        require_eq!(
            quote_reserve_debit,
            receipt.quote_amount_out,
            ErrorCode::MarketMathOverflow
        );
        let base_credit = token_account_credit(base_balance_before, &ctx.accounts.owner_base_account)?;
        let quote_credit = token_account_credit(quote_balance_before, &ctx.accounts.owner_quote_account)?;
        require_gte!(base_credit, args.min_base_amount_out, ErrorCode::SlippageExceeded);
        require_gte!(quote_credit, args.min_quote_amount_out, ErrorCode::SlippageExceeded);
        emit_cpi!(LiquidityRemoved {
            market: market_key,
            owner: owner_key,
            ylp_amount: receipt.ylp_amount,
            base_reserve_debit,
            quote_reserve_debit,
            base_owner_credit: base_credit,
            quote_owner_credit: quote_credit,
            ylp_supply: receipt.ylp_supply,
            base_live_reserve: ctx.accounts.market.base_side.reserves.live_reserve,
            quote_live_reserve: ctx.accounts.market.quote_side.reserves.live_reserve,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        let health = ctx.accounts.market.market_health()?;
        emit_cpi!(MarketHealthUpdated {
            market: market_key,
            global_health_base_contribution_for_quote_debt: health.global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: health.global_health_quote_contribution_for_base_debt,
            effective_base_debt_nad: health.effective_base_debt_nad,
            effective_quote_debt_nad: health.effective_quote_debt_nad,
            base_debt_health_bps: health.base_debt_health_bps,
            quote_debt_health_bps: health.quote_debt_health_bps,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });

        Ok(())
    }
}
