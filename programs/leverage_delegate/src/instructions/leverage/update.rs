use super::*;

#[derive(Accounts)]
#[instruction(args: UpdateLeverageOrderArgs)]
pub struct UpdateLeverageOrder<'info> {
    #[account(
        constraint = market.version == MARKET_LAYOUT_VERSION @ LeverageDelegateError::InvalidMarketVersion
    )]
    pub market: Box<Account<'info, Market>>,
    #[account(
        constraint = leverage_position.owner == owner.key() @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_position.market == market.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,
    #[account(
        mut,
        seeds = [
            ORDER_SEED_PREFIX,
            leverage_position.key().as_ref(),
            owner.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.owner == owner.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.market == market.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.position == leverage_position.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, LeverageOrder>>,
    #[account(mut)]
    pub owner: Signer<'info>,
}

impl<'info> UpdateLeverageOrder<'info> {
    pub fn handle_update(ctx: Context<Self>, args: UpdateLeverageOrderArgs) -> Result<()> {
        validate_order_kind(args.kind)?;
        require!(
            args.trigger_closeout_price_nad > 0
                && args.close_bps > 0
                && args.close_bps <= BPS_DENOMINATOR,
            LeverageDelegateError::InvalidOrder
        );
        let order = &mut ctx.accounts.order;
        order.kind = args.kind;
        order.trigger_closeout_price_nad = args.trigger_closeout_price_nad;
        order.close_bps = args.close_bps;
        reset_staged_settlement(order);
        Ok(())
    }
}
