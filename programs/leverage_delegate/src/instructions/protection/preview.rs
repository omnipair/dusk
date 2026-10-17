use super::*;

#[derive(Accounts)]
pub struct PreviewProtectionOrder<'info> {
    pub order: Box<Account<'info, ProtectionOrder>>,
    #[account(address = order.market)]
    pub market: Box<Account<'info, Market>>,
    #[account(constraint = borrow_position.key() == order.position @ LeverageDelegateError::InvalidOrder,
        constraint = borrow_position.owner == order.position_owner @ LeverageDelegateError::InvalidOrder,
        constraint = borrow_position.market == order.market @ LeverageDelegateError::InvalidOrder)]
    pub borrow_position: Option<Box<Account<'info, BorrowPosition>>>,
    #[account(constraint = leverage_position.key() == order.position @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_position.owner == order.position_owner @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_position.market == order.market @ LeverageDelegateError::InvalidOrder)]
    pub leverage_position: Option<Box<Account<'info, LeveragePosition>>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
}

impl PreviewProtectionOrder<'_> {
    pub fn handle(ctx: Context<Self>) -> Result<u64> {
        let debt_asset = MarketAsset::try_from_code(ctx.accounts.order.debt_asset)?;
        require_keys_eq!(
            ctx.accounts.collateral_mint.key(),
            ctx.accounts.market.side(debt_asset.opposite()).asset_mint,
            LeverageDelegateError::InvalidOrder
        );
        let clock = Clock::get()?;
        protection_health(
            &ctx.accounts.market.to_account_info(),
            ctx.accounts.borrow_position.as_deref().map(|p| &**p),
            ctx.accounts.leverage_position.as_deref().map(|p| &**p),
            ctx.accounts.order.action,
            debt_asset,
            &clock,
            true,
            dusk::instructions::leverage_collateral_fee(
                &ctx.accounts.collateral_mint,
                clock.epoch,
            )?,
        )
    }
}
