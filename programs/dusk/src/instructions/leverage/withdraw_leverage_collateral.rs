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
    instructions::common::{
        require_supported_asset_mint, token_account_credit, token_program_for_mint, validate_owner_asset_account,
    },
    shared::token::transfer_from_vault_to_user,
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct WithdrawLeverageCollateralArgs {
    pub debt_asset: u8,
    pub amount: u64,
    pub min_amount_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: WithdrawLeverageCollateralArgs)]
pub struct WithdrawLeverageCollateral<'info> {
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

    #[account(
        mut,
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            leverage_position.position_id.as_ref(),
        ],
        bump = leverage_position.bump,
        constraint = leverage_position.owner == owner.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.market == market.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.debt_asset == args.debt_asset @ ErrorCode::InvalidLeveragePosition,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,

    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [
            LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            collateral_mint.key().as_ref(),
        ],
        bump,
        constraint = leverage_collateral_vault.mint == collateral_mint.key() @ ErrorCode::InvalidVault,
        constraint = leverage_collateral_vault.owner == market.key() @ ErrorCode::InvalidVault
    )]
    pub leverage_collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> WithdrawLeverageCollateral<'info> {
    pub fn validate(&self, args: &WithdrawLeverageCollateralArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(args.amount > 0, ErrorCode::AmountZero);
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        require_keys_eq!(
            self.market.side(debt_asset.opposite())?.asset_mint,
            self.collateral_mint.key(),
            ErrorCode::InvalidMint
        );
        require_supported_asset_mint(&self.collateral_mint)?;
        validate_owner_asset_account(self.owner.key(), &self.collateral_mint, &self.owner_collateral_account)?;
        require_gte!(
            self.leverage_position.collateral_amount,
            args.amount,
            ErrorCode::InsufficientAmount
        );
        self.leverage_position.require_open()?;
        self.leverage_position
            .assert_position(self.owner.key(), self.market.key(), debt_asset)?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(WithdrawLeverageCollateralArgs);

    pub fn handle_withdraw(ctx: Context<'_, '_, '_, 'info, Self>, args: WithdrawLeverageCollateralArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let position_key = ctx.accounts.leverage_position.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let debt_mint_key = ctx.accounts.market.side(debt_asset)?.asset_mint;
        let collateral_mint_key = ctx.accounts.collateral_mint.key();

        let receipt = ctx
            .accounts
            .market
            .withdraw_leverage_collateral(&mut ctx.accounts.leverage_position, args.amount)?;
        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let owner_balance_before = ctx.accounts.owner_collateral_account.amount;
        transfer_from_vault_to_user(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.owner_collateral_account.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            args.amount,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.owner_collateral_account.reload()?;
        ctx.accounts.leverage_collateral_vault.reload()?;
        let amount_out = token_account_credit(owner_balance_before, &ctx.accounts.owner_collateral_account)?;
        require_gte!(amount_out, args.min_amount_out, ErrorCode::SlippageExceeded);

        let settlement_asset = ctx.accounts.leverage_position.settlement_asset()?;
        let settlement_asset_mint = ctx.accounts.market.side(settlement_asset)?.asset_mint;
        emit_cpi!(LeveragePositionUpdated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            margin_mode: ctx.accounts.leverage_position.margin_mode,
            margin_asset_mint: settlement_asset_mint,
            settlement_asset_mint,
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
