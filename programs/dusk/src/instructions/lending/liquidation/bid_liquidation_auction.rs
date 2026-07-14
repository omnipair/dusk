use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::log::emit_position_liquidated_low_heap,
    generate_market_seeds,
    math::risk::exponential_price_decay,
    shared::token::{
        get_transfer_fee, transfer_from_user_to_vault, transfer_from_vault_to_user, transfer_from_vault_to_vault,
    },
    state::{market::transitions::liquidation::LiquidationPricing, BorrowPosition, FutarchyAuthority, Market},
};

use super::common::validate_liquidation_accounts;
use crate::instructions::common::{require_supported_asset_mint, token_program_for_mint, validate_interest_accounts};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BidLiquidationAuctionArgs {
    pub repay_amount: u64,
    pub min_collateral_out: u64,
}

#[derive(Accounts)]
#[instruction(args: BidLiquidationAuctionArgs)]
pub struct BidLiquidationAuction<'info> {
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

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(mut)]
    pub liquidator: Signer<'info>,

    pub debt_asset_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub liquidator_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub liquidator_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,

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

impl<'info> BidLiquidationAuction<'info> {
    pub fn validate(&self, args: &BidLiquidationAuctionArgs) -> Result<()> {
        self.market.assert_started()?;
        require!(args.repay_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            self.liquidator_debt_account.amount,
            args.repay_amount,
            ErrorCode::InsufficientBalance
        );
        let debt_asset = validate_liquidation_accounts(
            &self.market,
            self.liquidator.key(),
            &self.debt_asset_mint,
            &self.collateral_asset_mint,
            &self.reserve_vault,
            &self.collateral_vault,
            &self.insurance_vault,
            &self.collateral_insurance_vault,
            &self.liquidator_debt_account,
            &self.liquidator_collateral_account,
        )?;
        let interest_asset = validate_interest_accounts(&self.market, &self.debt_asset_mint, &self.interest_vault)?;
        require!(interest_asset == debt_asset, ErrorCode::InvalidVault);
        require_supported_asset_mint(&self.debt_asset_mint)?;
        require_supported_asset_mint(&self.collateral_asset_mint)?;
        require_keys_eq!(
            self.borrow_position.market,
            self.market.key(),
            ErrorCode::InvalidBorrowPosition
        );
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(BidLiquidationAuctionArgs);

    pub fn handle_bid(ctx: Context<Self>, args: BidLiquidationAuctionArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let borrow_position_key = ctx.accounts.borrow_position.key();
        let borrower_key = ctx.accounts.borrow_position.owner;
        let liquidator_key = ctx.accounts.liquidator.key();
        let debt_asset_mint_key = ctx.accounts.debt_asset_mint.key();
        let collateral_asset_mint_key = ctx.accounts.collateral_asset_mint.key();
        let debt_asset = ctx.accounts.market.asset_for_mint(debt_asset_mint_key)?;

        require!(
            ctx.accounts.borrow_position.auction_start_time > 0,
            ErrorCode::PositionNotLiquidatable
        );

        let now = Clock::get()?.unix_timestamp;
        let elapsed_s = now.saturating_sub(ctx.accounts.borrow_position.auction_start_time);
        require!(elapsed_s >= 0, ErrorCode::MarketMathOverflow);
        let elapsed_ms = (elapsed_s as u64).saturating_mul(1000);

        let decayed_price = exponential_price_decay(
            ctx.accounts.borrow_position.auction_start_price_nad,
            elapsed_ms,
            300_000, // 5 minute half life
        )?;

        let floor_price = ctx.accounts.borrow_position.auction_floor_price_nad;

        let mut final_price = decayed_price.max(floor_price);

        // Liquidator pays LP fee (e.g. 0.20%) to beat the floor
        let reservation_fee = final_price
            .checked_mul(20)
            .and_then(|v| v.checked_div(10000))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        final_price = final_price
            .checked_add(reservation_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;

        let liquidation_pricing = LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad: final_price,
        };

        let liquidation_terms = ctx.accounts.market.liquidation_terms_with_pricing(
            &ctx.accounts.borrow_position,
            debt_asset,
            liquidation_pricing,
        )?;
        require_gte!(
            liquidation_terms.max_repay_amount,
            args.repay_amount,
            ErrorCode::LiquidationRepayTooLarge
        );

        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let debt_transfer_fee = get_transfer_fee(&ctx.accounts.debt_asset_mint.to_account_info(), args.repay_amount)?;
        let repay_credit = args
            .repay_amount
            .checked_sub(debt_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(repay_credit > 0, ErrorCode::AmountZero);
        transfer_from_user_to_vault(
            ctx.accounts.liquidator.to_account_info(),
            ctx.accounts.liquidator_debt_account.to_account_info(),
            ctx.accounts.reserve_vault.to_account_info(),
            ctx.accounts.debt_asset_mint.to_account_info(),
            debt_token_program.clone(),
            args.repay_amount,
            ctx.accounts.debt_asset_mint.decimals,
        )?;

        // For bids, there is no insurance draw or socialized loss since it's fully external.
        let liquidation_receipt = ctx.accounts.market.settle_liquidation(
            &mut ctx.accounts.borrow_position,
            debt_asset,
            repay_credit,
            0,
            0,
            0,
            liquidation_terms,
            liquidation_pricing,
        )?;

        if liquidation_receipt.interest_paid > 0 {
            transfer_from_vault_to_vault(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.reserve_vault.to_account_info(),
                ctx.accounts.interest_vault.to_account_info(),
                ctx.accounts.debt_asset_mint.to_account_info(),
                debt_token_program,
                liquidation_receipt.interest_paid,
                ctx.accounts.debt_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
            )?;
            ctx.accounts.interest_vault.reload()?;
            let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
            ctx.accounts.market.side_mut(debt_asset).record_interest_credit(
                liquidation_receipt.interest_paid,
                manager_fee_bps,
                ctx.accounts.futarchy_authority.revenue_share.interest_bps,
                ctx.accounts.futarchy_authority.protocol_auction_split,
            )?;
        }

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let collateral_credit = if liquidation_receipt.collateral_to_liquidator > 0 {
            let transfer_fee = get_transfer_fee(
                &ctx.accounts.collateral_asset_mint.to_account_info(),
                liquidation_receipt.collateral_to_liquidator,
            )?;
            let collateral_credit = liquidation_receipt
                .collateral_to_liquidator
                .checked_sub(transfer_fee)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require_gte!(collateral_credit, args.min_collateral_out, ErrorCode::SlippageExceeded);
            transfer_from_vault_to_user(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.collateral_vault.to_account_info(),
                ctx.accounts.liquidator_collateral_account.to_account_info(),
                ctx.accounts.collateral_asset_mint.to_account_info(),
                collateral_token_program.clone(),
                liquidation_receipt.collateral_to_liquidator,
                ctx.accounts.collateral_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
            )?;
            collateral_credit
        } else {
            0
        };
        require_gte!(collateral_credit, args.min_collateral_out, ErrorCode::SlippageExceeded);

        // Clear auction fields if full liquidation, else leave them for next bid
        if liquidation_receipt.remaining_debt == 0 {
            ctx.accounts.borrow_position.auction_start_time = 0;
            ctx.accounts.borrow_position.auction_start_price_nad = 0;
            ctx.accounts.borrow_position.auction_floor_price_nad = 0;
        }

        emit_position_liquidated_low_heap(
            market_key,
            borrow_position_key,
            borrower_key,
            liquidator_key,
            debt_asset_mint_key,
            collateral_asset_mint_key,
            liquidation_receipt.repaid_amount,
            liquidation_receipt.collateral_seized,
            liquidation_receipt.collateral_to_liquidator,
            liquidation_receipt.insurance_funded,
            liquidation_receipt.insurance_drawn,
            liquidation_receipt.socialized_loss,
            liquidation_receipt.remaining_debt,
        )?;
        Ok(())
    }
}
