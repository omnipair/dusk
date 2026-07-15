use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionLiquidated, MarketEventMetadata},
    generate_market_seeds,
    shared::token::{transfer_from_vault_to_user, transfer_from_vault_to_vault},
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset},
};

use super::common::{
    move_leverage_swap_fee, record_leverage_interest, validate_leverage_fee_account,
    validate_leverage_interest_account, validate_leverage_mints, validate_leverage_reserve_accounts,
};
use crate::instructions::common::{token_account_credit, token_program_for_mint};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct LiquidateLeverageArgs {
    pub debt_asset: u8,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: LiquidateLeverageArgs)]
/// Full-unwind liquidation for both margin modes.
///
/// This account set intentionally settles liquidator incentive and owner residual
/// in debt tokens. A collateral-settled path needs both debt and collateral
/// recipient accounts to preserve an atomic exact-output-to-writeoff fallback.
pub struct LiquidateLeverage<'info> {
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

    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    /// CHECK: Receives closed account rent and any non-incentive residual.
    #[account(mut, address = leverage_position.owner)]
    pub position_owner: AccountInfo<'info>,

    #[account(
        mut,
        close = position_owner,
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
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub debt_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,

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

    #[account(
        mut,
        constraint = liquidator_debt_account.mint == debt_mint.key() @ ErrorCode::InvalidTokenAccount,
        constraint = liquidator_debt_account.owner == liquidator.key() @ ErrorCode::InvalidTokenAccount,
    )]
    pub liquidator_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = owner_debt_account.mint == debt_mint.key() @ ErrorCode::InvalidTokenAccount,
        constraint = owner_debt_account.owner == position_owner.key() @ ErrorCode::InvalidTokenAccount,
    )]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub liquidator: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> LiquidateLeverage<'info> {
    pub fn validate(&self, args: &LiquidateLeverageArgs) -> Result<()> {
        self.market.assert_started()?;
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        validate_leverage_mints(&self.market, debt_asset, &self.debt_mint, &self.collateral_mint)?;
        validate_leverage_reserve_accounts(
            &self.market,
            debt_asset,
            &self.debt_mint,
            &self.collateral_mint,
            &self.debt_reserve_vault,
            &self.collateral_reserve_vault,
        )?;
        validate_leverage_fee_account(
            &self.market,
            &self.collateral_mint,
            &self.collateral_fee_vault,
            debt_asset.opposite(),
        )?;
        validate_leverage_interest_account(&self.market, &self.debt_mint, &self.debt_interest_vault, debt_asset)?;
        self.leverage_position.require_open()?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(LiquidateLeverageArgs);

    pub fn handle_liquidate(ctx: Context<'_, '_, '_, 'info, Self>, args: LiquidateLeverageArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let liquidator_key = ctx.accounts.liquidator.key();
        let owner_key = ctx.accounts.position_owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let position_key = ctx.accounts.leverage_position.key();
        let collateral_sold = ctx.accounts.leverage_position.collateral_amount;

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_from_vault_to_vault(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            collateral_sold,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        let swap = ctx
            .accounts
            .market
            .quote_leverage_swap(collateral_asset, collateral_sold)?;
        move_leverage_swap_fee(
            &ctx.accounts.market,
            &ctx.accounts.collateral_mint,
            &mut ctx.accounts.collateral_reserve_vault,
            &mut ctx.accounts.collateral_fee_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            swap.fee_credit,
        )?;

        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let receipt = ctx.accounts.market.liquidate_leverage(
            &mut ctx.accounts.leverage_position,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;

        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let liquidator_balance_before = ctx.accounts.liquidator_debt_account.amount;
        transfer_from_vault_to_user(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.liquidator_debt_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program.clone(),
            receipt.liquidator_amount,
            ctx.accounts.debt_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.liquidator_debt_account.reload()?;
        let liquidator_amount = token_account_credit(liquidator_balance_before, &ctx.accounts.liquidator_debt_account)?;

        let owner_balance_before = ctx.accounts.owner_debt_account.amount;
        transfer_from_vault_to_user(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            receipt.owner_residual,
            ctx.accounts.debt_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.owner_debt_account.reload()?;
        let owner_residual = token_account_credit(owner_balance_before, &ctx.accounts.owner_debt_account)?;

        record_leverage_interest(
            &mut ctx.accounts.market,
            debt_asset,
            &ctx.accounts.debt_mint,
            &mut ctx.accounts.debt_reserve_vault,
            &mut ctx.accounts.debt_interest_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.interest_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            receipt.interest_paid,
        )?;
        let margin_asset_mint = ctx
            .accounts
            .market
            .side(ctx.accounts.leverage_position.margin_asset()?)?
            .asset_mint;

        emit_cpi!(LeveragePositionLiquidated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            liquidator: liquidator_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            margin_mode: ctx.accounts.leverage_position.margin_mode,
            margin_asset_mint,
            // Full-unwind liquidation deliberately settles both margin modes in
            // debt tokens so insolvent positions can atomically use writeoff.
            settlement_asset_mint: debt_mint_key,
            debt_repaid: receipt.debt_repaid,
            interest_paid: receipt.interest_paid,
            principal_written_off: receipt.principal_written_off,
            collateral_sold: receipt.collateral_sold,
            closeout_value: receipt.closeout_value,
            liquidator_amount,
            owner_residual,
            metadata: MarketEventMetadata::new(liquidator_key, market_key)?,
        });
        Ok(())
    }
}
