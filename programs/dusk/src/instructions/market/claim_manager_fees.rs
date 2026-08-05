use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{ManagerFeesClaimed, MarketEventMetadata},
    generate_market_seeds,
    shared::token::transfer_checked_with_remaining_accounts,
    state::Market,
};

use crate::instructions::common::{
    require_reserve_custody, require_supported_asset_mint, token_program_for_mint, validate_interest_accounts,
    validate_owner_asset_account, validate_swap_fee_custody_accounts,
};

#[event_cpi]
#[derive(Accounts)]
pub struct ClaimManagerFees<'info> {
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

    #[account(mut)]
    pub manager: Signer<'info>,

    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub manager_asset_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> ClaimManagerFees<'info> {
    pub fn validate(&self) -> Result<()> {
        self.market.assert_manager(self.manager.key())?;

        // Bind both custody vaults to the same market side.
        let fee_asset = validate_swap_fee_custody_accounts(&self.market, &self.asset_mint, &self.reserve_vault)?;
        let interest_asset = validate_interest_accounts(&self.market, &self.asset_mint, &self.interest_vault)?;
        require!(fee_asset == interest_asset, ErrorCode::InvalidVault);

        // Reserve custody covers executable cash plus excluded swap fees.
        let market_side = self.market.side(fee_asset);
        require_gte!(
            self.reserve_vault.amount,
            market_side
                .reserves
                .cash_reserve
                .checked_add(market_side.fees.swap_fee_custody_balance)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            ErrorCode::UnbackedFeeLiability
        );
        validate_owner_asset_account(self.manager.key(), &self.asset_mint, &self.manager_asset_account)?;
        require_supported_asset_mint(&self.asset_mint)?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!();

    pub fn handle_claim(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let manager_key = ctx.accounts.manager.key();
        let asset_mint_key = ctx.accounts.asset_mint.key();
        let market_asset = ctx.accounts.market.asset_for_mint(asset_mint_key)?;

        // Snapshot both manager liabilities before moving their backing tokens.
        let (swap_fee_amount, interest_fee_amount) = {
            let market_side = ctx.accounts.market.side(market_asset);
            (
                market_side.fees.manager_swap_fee_liability,
                market_side.fees.manager_interest_fee_liability,
            )
        };
        require!(swap_fee_amount > 0 || interest_fee_amount > 0, ErrorCode::AmountZero);

        // Pay each fee component from the vault that physically holds it.
        let asset_token_program = token_program_for_mint(
            &ctx.accounts.asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        if swap_fee_amount > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.reserve_vault.to_account_info(),
                ctx.accounts.manager_asset_account.to_account_info(),
                ctx.accounts.asset_mint.to_account_info(),
                asset_token_program.clone(),
                swap_fee_amount,
                ctx.accounts.asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.reserve_vault.reload()?;
        }
        if interest_fee_amount > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.interest_vault.to_account_info(),
                ctx.accounts.manager_asset_account.to_account_info(),
                ctx.accounts.asset_mint.to_account_info(),
                asset_token_program,
                interest_fee_amount,
                ctx.accounts.asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
        }

        // Retire liabilities only after their transfers have succeeded.
        {
            let market_side = ctx.accounts.market.side_mut(market_asset);
            market_side.fees.manager_swap_fee_liability = 0;
            market_side.fees.manager_interest_fee_liability = 0;
            market_side.fees.swap_fee_custody_balance = market_side
                .fees
                .swap_fee_custody_balance
                .checked_sub(swap_fee_amount)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            market_side.fees.interest_vault_balance = market_side
                .fees
                .interest_vault_balance
                .checked_sub(interest_fee_amount)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            market_side.fees.assert_backed()?;
        }

        // Preserve executable reserve backing after removing swap-fee custody.
        require_reserve_custody(
            ctx.accounts.reserve_vault.amount,
            ctx.accounts.market.side(market_asset),
        )?;

        emit_cpi!(ManagerFeesClaimed {
            market: market_key,
            manager: manager_key,
            asset_mint: asset_mint_key,
            swap_fee_amount,
            interest_fee_amount,
            remaining_manager_swap_fee_liability: 0,
            remaining_manager_interest_fee_liability: 0,
            metadata: MarketEventMetadata::new(manager_key, market_key)?,
        });

        Ok(())
    }
}
