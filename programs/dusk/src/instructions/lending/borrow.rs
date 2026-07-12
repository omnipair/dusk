use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketDebtUpdated, MarketEventMetadata, MarketHealthUpdated},
    generate_market_seeds,
    shared::token::transfer_from_vault_to_user,
    state::{BorrowPosition, FutarchyAuthority, Market},
};

use crate::instructions::common::{require_supported_asset_mint, token_account_credit, token_program_for_mint};

use super::common::validate_borrow_accounts;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BorrowArgs {
    pub borrow_amount: u64,
    pub min_debt_amount_out: u64,
    pub min_health_bps: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: BorrowArgs)]
pub struct Borrow<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_mint.as_ref(),
            market.quote_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub debt_asset_mint: Box<InterfaceAccount<'info, Mint>>,

    pub collateral_asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            BORROW_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            borrow_position.position_id.as_ref(),
        ],
        bump = borrow_position.bump
    )]
    pub borrow_position: Box<Account<'info, BorrowPosition>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> Borrow<'info> {
    pub fn validate(&self, args: &BorrowArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(args.borrow_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            args.borrow_amount,
            args.min_debt_amount_out,
            ErrorCode::SlippageExceeded
        );
        validate_borrow_accounts(
            &self.market,
            self.owner.key(),
            &self.debt_asset_mint,
            &self.collateral_asset_mint,
            &self.reserve_vault,
            &self.owner_debt_account,
        )?;
        require_supported_asset_mint(&self.debt_asset_mint)?;
        self.borrow_position
            .assert_position(self.owner.key(), self.market.key())?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(BorrowArgs);

    pub fn handle_borrow(mut ctx: Context<Self>, args: BorrowArgs) -> Result<()> {
        let (market_key, owner_key, debt_asset_mint_key, debt_receipt) = {
            let accounts = &mut ctx.accounts;
            let market_key = accounts.market.key();
            let owner_key = accounts.owner.key();
            let debt_asset_mint_key = accounts.debt_asset_mint.key();
            let borrow_asset = accounts.market.asset_for_mint(debt_asset_mint_key)?;

            let debt_receipt = accounts.market.borrow(
                &mut accounts.borrow_position,
                borrow_asset,
                args.borrow_amount,
                args.min_health_bps,
            )?;

            let debt_token_program = token_program_for_mint(
                &accounts.debt_asset_mint,
                &accounts.token_program,
                &accounts.token_2022_program,
            )?;
            let owner_debt_balance_before = accounts.owner_debt_account.amount;

            transfer_from_vault_to_user(
                accounts.market.to_account_info(),
                accounts.reserve_vault.to_account_info(),
                accounts.owner_debt_account.to_account_info(),
                accounts.debt_asset_mint.to_account_info(),
                debt_token_program,
                args.borrow_amount,
                accounts.debt_asset_mint.decimals,
                &[&generate_market_seeds!(accounts.market)[..]],
            )?;
            accounts.owner_debt_account.reload()?;
            let debt_credit = token_account_credit(owner_debt_balance_before, &accounts.owner_debt_account)?;
            require_gte!(debt_credit, args.min_debt_amount_out, ErrorCode::SlippageExceeded);

            (market_key, owner_key, debt_asset_mint_key, debt_receipt)
        };

        emit_cpi!(MarketDebtUpdated {
            market: market_key,
            owner: owner_key,
            debt_asset_mint: debt_asset_mint_key,
            debt_delta: debt_receipt.debt_delta,
            fixed_base_debt: debt_receipt.fixed_base_debt,
            fixed_quote_debt: debt_receipt.fixed_quote_debt,
            base_debt_health_bps: debt_receipt.base_debt_health_bps,
            quote_debt_health_bps: debt_receipt.quote_debt_health_bps,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });

        let health = ctx.accounts.market.market_health()?;
        emit!(MarketHealthUpdated {
            market: market_key,
            recognized_base_collateral_for_quote_debt: health.recognized_base_collateral_for_quote_debt,
            recognized_quote_collateral_for_base_debt: health.recognized_quote_collateral_for_base_debt,
            effective_base_debt_nad: health.effective_base_debt_nad,
            effective_quote_debt_nad: health.effective_quote_debt_nad,
            base_debt_health_bps: health.base_debt_health_bps,
            quote_debt_health_bps: health.quote_debt_health_bps,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}
