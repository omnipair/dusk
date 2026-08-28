use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod instructions;
pub mod state;
pub(crate) mod token;

pub use constants::*;
pub use errors::*;
pub use instructions::*;
pub use state::*;

declare_id!("EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp");

#[program]
pub mod leverage_delegate {
    use super::*;

    pub fn create_leverage_order(
        ctx: Context<CreateLeverageOrder>,
        args: CreateLeverageOrderArgs,
    ) -> Result<()> {
        CreateLeverageOrder::handle_create(ctx, args)
    }

    pub fn update_leverage_order(
        ctx: Context<UpdateLeverageOrder>,
        args: UpdateLeverageOrderArgs,
    ) -> Result<()> {
        UpdateLeverageOrder::handle_update(ctx, args)
    }

    pub fn cancel_leverage_order(
        ctx: Context<CancelLeverageOrder>,
        _args: CancelLeverageOrderArgs,
    ) -> Result<()> {
        CancelLeverageOrder::handle_cancel(ctx)
    }

    pub fn before_take_profit(
        ctx: Context<BeforeLeverageOrder>,
        args: ExecuteOrderArgs,
    ) -> Result<()> {
        BeforeLeverageOrder::handle_before(ctx, args, ORDER_KIND_TAKE_PROFIT)
    }

    pub fn before_stop_loss(
        ctx: Context<BeforeLeverageOrder>,
        args: ExecuteOrderArgs,
    ) -> Result<()> {
        BeforeLeverageOrder::handle_before(ctx, args, ORDER_KIND_STOP_LOSS)
    }

    pub fn after_close_order<'info>(
        ctx: Context<'_, '_, '_, 'info, AfterCloseOrder<'info>>,
        args: ExecuteOrderArgs,
    ) -> Result<()> {
        AfterCloseOrder::handle_after(ctx, args)
    }

    pub fn create_leverage_entry_order<'info>(
        ctx: Context<'_, '_, '_, 'info, CreateLeverageEntryOrder<'info>>,
        args: CreateLeverageEntryOrderArgs,
    ) -> Result<()> {
        CreateLeverageEntryOrder::handle_create(ctx, args)
    }

    pub fn cancel_leverage_entry_order<'info>(
        ctx: Context<'_, '_, '_, 'info, CancelLeverageEntryOrder<'info>>,
        args: LeverageEntryOrderIdArgs,
    ) -> Result<()> {
        CancelLeverageEntryOrder::handle_cancel(ctx, args)
    }

    pub fn execute_leverage_entry_order<'info>(
        ctx: Context<'_, '_, '_, 'info, ExecuteLeverageEntryOrder<'info>>,
        args: LeverageEntryOrderIdArgs,
    ) -> Result<()> {
        ExecuteLeverageEntryOrder::handle_execute(ctx, args)
    }

    pub fn create_hlp_order<'info>(
        ctx: Context<'_, '_, '_, 'info, CreateHlpOrder<'info>>,
        args: CreateHlpOrderArgs,
    ) -> Result<()> {
        CreateHlpOrder::handle_create(ctx, args)
    }

    pub fn cancel_hlp_order<'info>(
        ctx: Context<'_, '_, '_, 'info, CancelHlpOrder<'info>>,
        args: HlpOrderIdArgs,
    ) -> Result<()> {
        CancelHlpOrder::handle_cancel(ctx, args)
    }

    pub fn execute_hlp_order<'info>(
        ctx: Context<'_, '_, '_, 'info, ExecuteHlpOrder<'info>>,
        args: HlpOrderIdArgs,
    ) -> Result<()> {
        ExecuteHlpOrder::handle_execute(ctx, args)
    }

    pub fn settle_hlp_order_yield<'info>(
        ctx: Context<'_, '_, '_, 'info, SettleHlpOrderYield<'info>>,
        args: HlpOrderIdArgs,
    ) -> Result<()> {
        SettleHlpOrderYield::handle_settle(ctx, args)
    }
}
