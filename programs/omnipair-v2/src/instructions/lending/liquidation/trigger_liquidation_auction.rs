use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::*,
    errors::ErrorCode,
    state::{market::transitions::liquidation::LiquidationPricing, BorrowPosition, Market},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct TriggerLiquidationAuctionArgs {}

#[derive(Accounts)]
#[instruction(args: TriggerLiquidationAuctionArgs)]
pub struct TriggerLiquidationAuction<'info> {
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
        mut,
        seeds = [
            BORROW_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            borrow_position.position_id.as_ref(),
        ],
        bump = borrow_position.bump
    )]
    pub borrow_position: Box<Account<'info, BorrowPosition>>,

    pub debt_asset_mint: Box<InterfaceAccount<'info, Mint>>,
}

impl<'info> TriggerLiquidationAuction<'info> {
    pub fn validate(&self, _args: &TriggerLiquidationAuctionArgs) -> Result<()> {
        self.market.assert_started()?;
        require_keys_eq!(
            self.borrow_position.market,
            self.market.key(),
            ErrorCode::InvalidBorrowPosition
        );
        Ok(())
    }

    pub fn update(&mut self) -> Result<()> {
        self.market.update()
    }

    pub fn update_and_validate(&mut self, args: &TriggerLiquidationAuctionArgs) -> Result<()> {
        self.update()?;
        self.validate(args)
    }

    pub fn handle_trigger(ctx: Context<Self>, _args: TriggerLiquidationAuctionArgs) -> Result<()> {
        let debt_asset_mint_key = ctx.accounts.debt_asset_mint.key();
        let debt_asset = ctx.accounts.market.asset_for_mint(debt_asset_mint_key)?;

        let liquidation_reference_price_nad = ctx
            .accounts
            .market
            .liquidation_reference_price_nad(debt_asset)?;

        let liquidation_pricing = LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad: liquidation_reference_price_nad,
        };

        let health_bps = ctx.accounts.market.liquidation_health_bps_with_pricing(
            &ctx.accounts.borrow_position,
            debt_asset,
            liquidation_pricing,
        )?;

        require!(
            health_bps < ctx.accounts.market.config.market_health_min_bps as u64,
            ErrorCode::PositionNotLiquidatable
        );

        require!(
            ctx.accounts.borrow_position.auction_start_time == 0,
            ErrorCode::PositionNotLiquidatable
        );

        let floor_price = liquidation_reference_price_nad;
        let start_price = floor_price
            .checked_mul(105)
            .and_then(|v| v.checked_div(100))
            .ok_or(ErrorCode::MarketMathOverflow)?;

        ctx.accounts.borrow_position.auction_start_time = Clock::get()?.unix_timestamp;
        ctx.accounts.borrow_position.auction_start_price_nad = start_price;
        ctx.accounts.borrow_position.auction_floor_price_nad = floor_price;

        Ok(())
    }
}
