use super::*;

#[derive(Accounts)]
#[instruction(args: CancelLeverageOrderArgs)]
pub struct CancelLeverageOrder<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            ORDER_SEED_PREFIX,
            order.position.as_ref(),
            owner.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.owner == owner.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, LeverageOrder>>,
    #[account(mut)]
    pub owner: Signer<'info>,
}

impl<'info> CancelLeverageOrder<'info> {
    pub fn handle_cancel(_ctx: Context<Self>) -> Result<()> {
        Ok(())
    }
}
