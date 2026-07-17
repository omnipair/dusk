use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionUpdated, MarketEventMetadata},
    generate_market_seeds,
    shared::token::transfer_from_vault_to_user,
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset},
};

use super::common::validate_owner_debt_account;
use crate::instructions::common::{
    require_supported_asset_mint, token_account_credit, token_program_for_mint, validate_side_vault_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct RemoveLeverageMarginArgs {
    pub debt_asset: u8,
    pub amount: u64,
    pub min_amount_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: RemoveLeverageMarginArgs)]
pub struct RemoveLeverageMargin<'info> {
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

    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(address = leverage_position.owner)]
    /// CHECK: Owner address bound by leverage_position.
    pub position_owner: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            leverage_position.position_id.as_ref(),
        ],
        bump = leverage_position.bump,
        constraint = leverage_position.market == market.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.debt_asset == args.debt_asset @ ErrorCode::InvalidLeveragePosition,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,

    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> RemoveLeverageMargin<'info> {
    pub fn validate(&self, args: &RemoveLeverageMarginArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require_keys_eq!(self.owner.key(), self.position_owner.key(), ErrorCode::InvalidSigner);
        require!(args.amount > 0, ErrorCode::AmountZero);
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        validate_side_vault_accounts(&self.market, debt_asset, &self.debt_mint, &self.debt_reserve_vault)?;
        validate_owner_debt_account(self.owner.key(), &self.debt_mint, &self.owner_debt_account)?;
        require_supported_asset_mint(&self.debt_mint)?;
        self.leverage_position.require_open()?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(RemoveLeverageMarginArgs);

    pub fn handle_remove_margin(ctx: Context<'_, '_, '_, 'info, Self>, args: RemoveLeverageMarginArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let position_key = ctx.accounts.leverage_position.key();

        let receipt = ctx
            .accounts
            .market
            .remove_leverage_margin(&mut ctx.accounts.leverage_position, args.amount)?;
        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let owner_balance_before = ctx.accounts.owner_debt_account.amount;
        transfer_from_vault_to_user(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            args.amount,
            ctx.accounts.debt_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.owner_debt_account.reload()?;
        let amount_out = token_account_credit(owner_balance_before, &ctx.accounts.owner_debt_account)?;
        require_gte!(amount_out, args.min_amount_out, ErrorCode::SlippageExceeded);

        emit_cpi!(LeveragePositionUpdated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: ctx.accounts.market.side(debt_asset.opposite()).asset_mint,
            requested_principal: receipt.requested_principal,
            referral_fee_amount: receipt.referral_fee_amount,
            gross_debt_delta: receipt.gross_debt_delta,
            debt_delta: receipt.debt_delta,
            collateral_delta: receipt.collateral_delta,
            debt_amount: receipt.debt_amount,
            debt_shares: receipt.debt_shares,
            collateral_amount: receipt.collateral_amount,
            closeout_value: receipt.closeout_value,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}
