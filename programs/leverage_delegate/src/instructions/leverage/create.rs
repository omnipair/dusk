use super::*;

#[derive(Accounts)]
#[instruction(args: CreateLeverageOrderArgs)]
pub struct CreateLeverageOrder<'info> {
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
        init,
        payer = owner,
        space = 8 + LeverageOrder::INIT_SPACE,
        seeds = [
            ORDER_SEED_PREFIX,
            leverage_position.key().as_ref(),
            owner.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump
    )]
    pub order: Box<Account<'info, LeverageOrder>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

impl<'info> CreateLeverageOrder<'info> {
    pub fn handle_create(ctx: Context<Self>, args: CreateLeverageOrderArgs) -> Result<()> {
        validate_order_kind(args.kind)?;
        require!(
            args.trigger_closeout_price_nad > 0
                && args.close_bps > 0
                && args.close_bps <= BPS_DENOMINATOR,
            LeverageDelegateError::InvalidOrder
        );
        let order = &mut ctx.accounts.order;
        order.owner = ctx.accounts.owner.key();
        order.market = ctx.accounts.market.key();
        order.position = ctx.accounts.leverage_position.key();
        order.order_id = args.order_id;
        order.kind = args.kind;
        order.trigger_closeout_price_nad = args.trigger_closeout_price_nad;
        order.close_bps = args.close_bps;
        reset_staged_settlement(order);
        order.bump = ctx.bumps.order;
        Ok(())
    }
}
