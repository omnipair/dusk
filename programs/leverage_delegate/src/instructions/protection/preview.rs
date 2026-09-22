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
}

impl PreviewProtectionOrder<'_> {
    pub fn handle(ctx: Context<Self>) -> Result<u64> {
        protection_health(
            &ctx.accounts.market.to_account_info(),
            ctx.accounts.borrow_position.as_deref().map(|p| &**p),
            ctx.accounts.leverage_position.as_deref().map(|p| &**p),
            ctx.accounts.order.action,
            MarketAsset::try_from_code(ctx.accounts.order.debt_asset)?,
            &Clock::get()?,
            true,
        )
    }
}
