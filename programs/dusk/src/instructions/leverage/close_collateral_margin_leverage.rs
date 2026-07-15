use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionClosed, MarketEventMetadata},
    generate_market_seeds,
    instructions::common::{token_account_credit, token_program_for_mint, validate_owner_asset_account},
    shared::token::{transfer_from_vault_to_user, transfer_from_vault_to_vault},
    state::{FutarchyAuthority, LeverageMarginMode, LeveragePosition, Market, MarketAsset},
};

use super::common::{
    leverage_transfer_amount_for_credit, move_leverage_swap_fee, record_leverage_interest,
    validate_leverage_fee_account, validate_leverage_interest_account, validate_leverage_mints,
    validate_leverage_reserve_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseCollateralMarginLeverageArgs {
    pub debt_asset: u8,
    pub max_collateral_in: u64,
    pub min_residual_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: CloseCollateralMarginLeverageArgs)]
pub struct CloseCollateralMarginLeverage<'info> {
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
        close = owner,
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

    #[account(mut)]
    pub owner_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CloseCollateralMarginLeverage<'info> {
    pub fn validate(&self, args: &CloseCollateralMarginLeverageArgs) -> Result<()> {
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
        validate_owner_asset_account(self.owner.key(), &self.collateral_mint, &self.owner_collateral_account)?;
        self.leverage_position.require_open()?;
        self.leverage_position
            .assert_position(self.owner.key(), self.market.key(), debt_asset)?;
        self.leverage_position
            .require_margin_mode(LeverageMarginMode::Collateral)?;

        let debt_amount = self.leverage_position.debt_amount(&self.market.debt)?;
        let swap = self
            .market
            .quote_leverage_swap_exact_output(debt_asset.opposite(), debt_amount)?;
        let collateral_debit = leverage_transfer_amount_for_credit(&self.collateral_mint, swap.amount_in)?;
        require_gte!(args.max_collateral_in, collateral_debit, ErrorCode::SlippageExceeded);
        require_gte!(
            self.leverage_position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(CloseCollateralMarginLeverageArgs);

    pub fn handle_close(ctx: Context<'_, '_, '_, 'info, Self>, args: CloseCollateralMarginLeverageArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let position_key = ctx.accounts.leverage_position.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let debt_amount = ctx.accounts.leverage_position.debt_amount(&ctx.accounts.market.debt)?;
        let swap = ctx
            .accounts
            .market
            .quote_leverage_swap_exact_output(collateral_asset, debt_amount)?;
        let collateral_debit = leverage_transfer_amount_for_credit(&ctx.accounts.collateral_mint, swap.amount_in)?;
        require_gte!(args.max_collateral_in, collateral_debit, ErrorCode::SlippageExceeded);
        require_gte!(
            ctx.accounts.leverage_position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let reserve_balance_before = ctx.accounts.collateral_reserve_vault.amount;
        transfer_from_vault_to_vault(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program.clone(),
            collateral_debit,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        ctx.accounts.leverage_collateral_vault.reload()?;
        let reserve_credit = token_account_credit(reserve_balance_before, &ctx.accounts.collateral_reserve_vault)?;
        require_eq!(reserve_credit, swap.amount_in, ErrorCode::UnexpectedTokenTransferAmount);

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
        let receipt = ctx.accounts.market.close_collateral_margin_leverage(
            &mut ctx.accounts.leverage_position,
            collateral_debit,
            args.max_collateral_in,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;

        let owner_balance_before = ctx.accounts.owner_collateral_account.amount;
        transfer_from_vault_to_user(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.owner_collateral_account.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            receipt.residual,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.owner_collateral_account.reload()?;
        ctx.accounts.leverage_collateral_vault.reload()?;
        let residual_credit = token_account_credit(owner_balance_before, &ctx.accounts.owner_collateral_account)?;
        require_gte!(residual_credit, args.min_residual_out, ErrorCode::SlippageExceeded);

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

        emit_cpi!(LeveragePositionClosed {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            margin_mode: LeverageMarginMode::Collateral.code(),
            margin_asset_mint: collateral_mint_key,
            settlement_asset_mint: collateral_mint_key,
            debt_repaid: receipt.debt_repaid,
            interest_paid: receipt.interest_paid,
            collateral_sold: receipt.collateral_sold,
            closeout_value: receipt.closeout_value,
            residual: residual_credit,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}
