use anchor_lang::{prelude::*, solana_program::program::set_return_data};
use anchor_spl::{
    token::{self, Token},
    token_2022::{self, Token2022},
    token_interface::{Mint, TokenAccount},
};
use dusk::{
    constants::{BPS_DENOMINATOR, MARKET_LAYOUT_VERSION, NAD},
    instructions::{
        ClaimYieldArgs, LeverageDelegationApproval, SetYieldRecipientArgs, WithdrawSingleSidedArgs,
        LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_CLOSE_SETTLED,
    },
    math::numerics::ceil_div,
    program::Dusk,
    state::{
        FutarchyAuthority, LeverageDelegation, LeveragePosition, Market, MarketAsset, YieldAccount,
        YieldTokenKind,
    },
    token::get_transfer_fee,
};
use std::cmp::min;

mod entry;
pub use entry::*;

declare_id!("EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp");

pub const ORDER_SEED_PREFIX: &[u8] = b"leverage_order";
pub const EXECUTOR_INCENTIVE_BPS: u64 = 500;
pub const ORDER_KIND_TAKE_PROFIT: u8 = 1;
pub const ORDER_KIND_STOP_LOSS: u8 = 2;
pub const HLP_ORDER_SEED_PREFIX: &[u8] = b"hlp_order";
pub const HLP_ORDER_KIND_STOP_LOSS: u8 = 1;
pub const HLP_ORDER_KIND_STOP_RATE: u8 = 2;
pub const HLP_ORDER_STATUS_ACTIVE: u8 = 0;
pub const HLP_ORDER_STATUS_CANCELLED: u8 = 1;
pub const HLP_ORDER_STATUS_EXECUTED: u8 = 2;
pub const ENTRY_ORDER_SEED_PREFIX: &[u8] = b"leverage_entry_order";

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

    pub fn after_close_order(ctx: Context<AfterCloseOrder>, args: ExecuteOrderArgs) -> Result<()> {
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateLeverageOrderArgs {
    pub order_id: u64,
    pub kind: u8,
    pub trigger_closeout_price_nad: u64,
    /// Portion of the current position closed when triggered. `10_000` is a
    /// full close; smaller values realize one proportional slice.
    pub close_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateLeverageOrderArgs {
    pub order_id: u64,
    pub kind: u8,
    pub trigger_closeout_price_nad: u64,
    pub close_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CancelLeverageOrderArgs {
    pub order_id: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ExecuteOrderArgs {
    pub order_id: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateHlpOrderArgs {
    pub order_id: u64,
    pub kind: u8,
    pub hlp_amount: u64,
    /// Stop Loss: principal NAV per hLP token in NAD. Stop Rate: opposite
    /// funding APR in NAD (NAD == 100% APR).
    pub trigger_nad: u64,
    pub min_target_amount_out: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct HlpOrderIdArgs {
    pub order_id: u64,
}

#[account]
#[derive(InitSpace)]
pub struct LeverageOrder {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position: Pubkey,
    pub order_id: u64,
    pub kind: u8,
    pub trigger_closeout_price_nad: u64,
    pub close_bps: u16,
    pub staged_margin: u64,
    pub staged_collateral_amount: u64,
    pub staged_remaining_collateral_amount: u64,
    pub staged_remaining_debt_shares: u128,
    pub staged_remaining_debt_principal: u128,
    pub staged_custody_token_account: Pubkey,
    pub staged_output_mint: Pubkey,
    pub staged_output_amount: u64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct HlpOrder {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub target_hlp_mint: Pubkey,
    pub custody_hlp_account: Pubkey,
    pub order_id: u64,
    pub kind: u8,
    pub status: u8,
    pub hlp_amount: u64,
    pub trigger_nad: u64,
    pub min_target_amount_out: u64,
    pub bump: u8,
}

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

#[derive(Accounts)]
#[instruction(args: ExecuteOrderArgs)]
pub struct BeforeLeverageOrder<'info> {
    #[account(
        mut,
        seeds = [
            ORDER_SEED_PREFIX,
            leverage_position.key().as_ref(),
            order.owner.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.market == market.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.position == leverage_position.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, LeverageOrder>>,
    #[account(
        constraint = market.version == MARKET_LAYOUT_VERSION @ LeverageDelegateError::InvalidMarketVersion
    )]
    pub market: Box<Account<'info, Market>>,
    #[account(
        constraint = leverage_position.owner == order.owner @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_position.market == market.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,
    #[account(
        constraint = leverage_delegation.owner == order.owner @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_delegation.market == market.key() @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_delegation.position == leverage_position.key() @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_delegation.debt_asset == leverage_position.debt_asset @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_delegation.delegated_program == crate::ID @ LeverageDelegateError::InvalidOrder,
    )]
    pub leverage_delegation: Box<Account<'info, LeverageDelegation>>,
    #[account(
        constraint = custody_token_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Collateral mint is needed to reproduce the exact net reserve credit for
    /// Token-2022 transfer-fee assets before approving a partial close.
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    pub executor: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(args: ExecuteOrderArgs)]
pub struct AfterCloseOrder<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            ORDER_SEED_PREFIX,
            order.position.as_ref(),
            order.owner.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
    )]
    pub order: Box<Account<'info, LeverageOrder>>,
    /// CHECK: Order owner receives closed account rent.
    #[account(mut, address = order.owner)]
    pub owner: AccountInfo<'info>,
    #[account(
        constraint = leverage_position.key() == order.position @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_position.owner == order.owner @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_position.market == order.market @ LeverageDelegateError::InvalidOrder,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,
    #[account(
        constraint = leverage_delegation.owner == order.owner @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_delegation.market == order.market @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_delegation.position == order.position @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_delegation.debt_asset == leverage_position.debt_asset @ LeverageDelegateError::InvalidOrder,
        constraint = leverage_delegation.delegated_program == crate::ID @ LeverageDelegateError::InvalidOrder,
    )]
    pub leverage_delegation: Box<Account<'info, LeverageDelegation>>,
    #[account(
        mut,
        constraint = custody_token_account.key() == order.staged_custody_token_account @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_token_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = executor_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub executor_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_token_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        constraint = token_mint.key() == order.staged_output_mint @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    pub executor: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
#[instruction(args: CreateHlpOrderArgs)]
pub struct CreateHlpOrder<'info> {
    #[account(
        constraint = market.version == MARKET_LAYOUT_VERSION @ LeverageDelegateError::InvalidMarketVersion
    )]
    pub market: Box<Account<'info, Market>>,
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = owner,
        space = 8 + HlpOrder::INIT_SPACE,
        seeds = [
            HLP_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            target_hlp_mint.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump
    )]
    pub order: Box<Account<'info, HlpOrder>>,
    #[account(
        mut,
        constraint = owner_hlp_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = custody_hlp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    /// CHECK: Canonical Dusk CPI-event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(args: HlpOrderIdArgs)]
pub struct CancelHlpOrder<'info> {
    #[account(
        mut,
        seeds = [
            HLP_ORDER_SEED_PREFIX,
            order.market.as_ref(),
            owner.key().as_ref(),
            order.target_hlp_mint.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.owner == owner.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.status == HLP_ORDER_STATUS_ACTIVE @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, HlpOrder>>,
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        address = order.custody_hlp_account,
        constraint = custody_hlp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_hlp_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub owner: Signer<'info>,
    pub token_2022_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
#[instruction(args: HlpOrderIdArgs)]
pub struct ExecuteHlpOrder<'info> {
    #[account(
        mut,
        seeds = [
            HLP_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            order.owner.as_ref(),
            order.target_hlp_mint.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.market == market.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.status == HLP_ORDER_STATUS_ACTIVE @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, HlpOrder>>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, address = order.target_hlp_mint)]
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub borrowed_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = custody_target_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        address = order.custody_hlp_account,
        constraint = custody_hlp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub hlp_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(
        mut,
        constraint = owner_target_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = executor_target_account.owner == executor.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub executor_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub executor: Signer<'info>,
    /// CHECK: Canonical Dusk CPI-event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
#[instruction(args: HlpOrderIdArgs)]
pub struct SettleHlpOrderYield<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            HLP_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            order.owner.as_ref(),
            order.target_hlp_mint.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.market == market.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.status != HLP_ORDER_STATUS_ACTIVE @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, HlpOrder>>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        address = order.custody_hlp_account,
        constraint = custody_hlp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_hlp_account.amount == 0 @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(
        mut,
        constraint = owner_base_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_base_account.mint == base_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_base_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_quote_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_quote_account.mint == quote_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_quote_account: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Order owner receives account rent; yield recipients are checked by Dusk.
    #[account(mut, address = order.owner)]
    pub owner: AccountInfo<'info>,
    /// CHECK: Canonical Dusk CPI-event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CreateHlpOrder<'info> {
    pub fn handle_create(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CreateHlpOrderArgs,
    ) -> Result<()> {
        validate_hlp_order_kind(args.kind)?;
        require!(
            args.hlp_amount > 0 && args.trigger_nad > 0,
            LeverageDelegateError::InvalidOrder
        );
        ctx.accounts
            .market
            .asset_for_hlp_mint(ctx.accounts.target_hlp_mint.key())?;
        require_keys_eq!(
            ctx.accounts.base_mint.key(),
            ctx.accounts.market.base_side.asset_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.quote_mint.key(),
            ctx.accounts.market.quote_side.asset_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_gte!(
            ctx.accounts.owner_hlp_account.amount,
            args.hlp_amount,
            LeverageDelegateError::InvalidTokenAccount
        );
        require!(
            ctx.accounts.custody_hlp_account.amount == 0,
            LeverageDelegateError::InvalidTokenAccount
        );

        validate_hlp_yield_account(
            &ctx.accounts.base_yield_account,
            ctx.accounts.order.key(),
            ctx.accounts.market.key(),
            ctx.accounts.target_hlp_mint.key(),
            ctx.accounts.base_mint.key(),
        )?;
        validate_hlp_yield_account(
            &ctx.accounts.quote_yield_account,
            ctx.accounts.order.key(),
            ctx.accounts.market.key(),
            ctx.accounts.target_hlp_mint.key(),
            ctx.accounts.quote_mint.key(),
        )?;

        {
            let order = &mut ctx.accounts.order;
            order.owner = ctx.accounts.owner.key();
            order.market = ctx.accounts.market.key();
            order.target_hlp_mint = ctx.accounts.target_hlp_mint.key();
            order.custody_hlp_account = ctx.accounts.custody_hlp_account.key();
            order.order_id = args.order_id;
            order.kind = args.kind;
            order.status = HLP_ORDER_STATUS_ACTIVE;
            order.hlp_amount = args.hlp_amount;
            order.trigger_nad = args.trigger_nad;
            order.min_target_amount_out = args.min_target_amount_out;
            order.bump = ctx.bumps.order;
        }

        let market_key = ctx.accounts.order.market;
        let owner_key = ctx.accounts.order.owner;
        let target_hlp_mint_key = ctx.accounts.order.target_hlp_mint;
        let order_id_bytes = ctx.accounts.order.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let authority_seeds = &[
            HLP_ORDER_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            target_hlp_mint_key.as_ref(),
            &order_id_bytes,
            &bump_seed,
        ];
        let signer = &[&authority_seeds[..]];

        set_hlp_yield_recipient(
            ctx.accounts.dusk_program.to_account_info(),
            ctx.accounts.dusk_event_authority.to_account_info(),
            ctx.accounts.market.to_account_info(),
            ctx.accounts.order.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            ctx.accounts.base_yield_account.to_account_info(),
            owner_key,
            signer,
        )?;
        set_hlp_yield_recipient(
            ctx.accounts.dusk_program.to_account_info(),
            ctx.accounts.dusk_event_authority.to_account_info(),
            ctx.accounts.market.to_account_info(),
            ctx.accounts.order.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            ctx.accounts.quote_yield_account.to_account_info(),
            owner_key,
            signer,
        )?;

        token_2022::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_2022_program.to_account_info(),
                token_2022::TransferChecked {
                    from: ctx.accounts.owner_hlp_account.to_account_info(),
                    mint: ctx.accounts.target_hlp_mint.to_account_info(),
                    to: ctx.accounts.custody_hlp_account.to_account_info(),
                    authority: ctx.accounts.owner.to_account_info(),
                },
            )
            .with_remaining_accounts(ctx.remaining_accounts.to_vec()),
            args.hlp_amount,
            ctx.accounts.target_hlp_mint.decimals,
        )?;
        ctx.accounts.custody_hlp_account.reload()?;
        require_eq!(
            ctx.accounts.custody_hlp_account.amount,
            args.hlp_amount,
            LeverageDelegateError::InvalidTokenAccount
        );
        Ok(())
    }
}

impl<'info> CancelHlpOrder<'info> {
    pub fn handle_cancel(
        ctx: Context<'_, '_, '_, 'info, Self>,
        _args: HlpOrderIdArgs,
    ) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.target_hlp_mint.key(),
            ctx.accounts.order.target_hlp_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_eq!(
            ctx.accounts.custody_hlp_account.amount,
            ctx.accounts.order.hlp_amount,
            LeverageDelegateError::InvalidTokenAccount
        );
        let market_key = ctx.accounts.order.market;
        let owner_key = ctx.accounts.order.owner;
        let target_hlp_mint_key = ctx.accounts.order.target_hlp_mint;
        let order_id_bytes = ctx.accounts.order.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let authority_seeds = &[
            HLP_ORDER_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            target_hlp_mint_key.as_ref(),
            &order_id_bytes,
            &bump_seed,
        ];
        token_2022::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_2022_program.to_account_info(),
                token_2022::TransferChecked {
                    from: ctx.accounts.custody_hlp_account.to_account_info(),
                    mint: ctx.accounts.target_hlp_mint.to_account_info(),
                    to: ctx.accounts.owner_hlp_account.to_account_info(),
                    authority: ctx.accounts.order.to_account_info(),
                },
                &[&authority_seeds[..]],
            )
            .with_remaining_accounts(ctx.remaining_accounts.to_vec()),
            ctx.accounts.order.hlp_amount,
            ctx.accounts.target_hlp_mint.decimals,
        )?;
        ctx.accounts.custody_hlp_account.reload()?;
        require_eq!(
            ctx.accounts.custody_hlp_account.amount,
            0,
            LeverageDelegateError::InvalidTokenAccount
        );
        ctx.accounts.order.status = HLP_ORDER_STATUS_CANCELLED;
        Ok(())
    }
}

impl<'info> ExecuteHlpOrder<'info> {
    pub fn handle_execute(
        ctx: Context<'_, '_, '_, 'info, Self>,
        _args: HlpOrderIdArgs,
    ) -> Result<()> {
        require!(
            ctx.accounts.market.version == MARKET_LAYOUT_VERSION,
            LeverageDelegateError::InvalidMarketVersion
        );
        require_eq!(
            ctx.accounts.custody_hlp_account.amount,
            ctx.accounts.order.hlp_amount,
            LeverageDelegateError::InvalidTokenAccount
        );
        require!(
            ctx.accounts.custody_target_account.amount == 0,
            LeverageDelegateError::InvalidTokenAccount
        );

        let target_asset = ctx
            .accounts
            .market
            .asset_for_hlp_mint(ctx.accounts.target_hlp_mint.key())?;
        let trigger_preview =
            preview_hlp_order_trigger(ctx.accounts, target_asset, ctx.accounts.order.hlp_amount)?;

        let target_mint = match target_asset {
            MarketAsset::Base => ctx.accounts.base_mint.key(),
            MarketAsset::Quote => ctx.accounts.quote_mint.key(),
        };
        require_keys_eq!(
            ctx.accounts.custody_target_account.mint,
            target_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.owner_target_account.mint,
            target_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.executor_target_account.mint,
            target_mint,
            LeverageDelegateError::InvalidTokenAccount
        );

        let (principal_nav, funding_apr) = match ctx.accounts.order.kind {
            HLP_ORDER_KIND_STOP_LOSS => (trigger_preview.principal_nav_per_token_nad, 0),
            HLP_ORDER_KIND_STOP_RATE => (0, trigger_preview.funding_apr_ema_nad),
            _ => return err!(LeverageDelegateError::InvalidOrder),
        };
        require!(
            hlp_order_trigger_met(
                ctx.accounts.order.kind,
                principal_nav,
                funding_apr,
                ctx.accounts.order.trigger_nad,
            )?,
            LeverageDelegateError::TriggerNotMet
        );

        withdraw_hlp_order_position(ctx.accounts, ctx.remaining_accounts)?;
        ctx.accounts.custody_target_account.reload()?;
        let output = ctx.accounts.custody_target_account.amount;
        require_gte!(
            output,
            ctx.accounts.order.min_target_amount_out,
            LeverageDelegateError::InvalidOrder
        );
        let incentive = min(
            output,
            ceil_div(
                (output as u128)
                    .checked_mul(EXECUTOR_INCENTIVE_BPS as u128)
                    .ok_or(LeverageDelegateError::MathOverflow)?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(LeverageDelegateError::MathOverflow)? as u64,
        );
        let owner_amount = output
            .checked_sub(incentive)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        require_gte!(
            owner_amount,
            ctx.accounts.order.min_target_amount_out,
            LeverageDelegateError::InvalidOrder
        );
        let market_key = ctx.accounts.order.market;
        let owner_key = ctx.accounts.order.owner;
        let target_hlp_mint_key = ctx.accounts.order.target_hlp_mint;
        let order_id_bytes = ctx.accounts.order.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let authority_seeds = &[
            HLP_ORDER_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            target_hlp_mint_key.as_ref(),
            &order_id_bytes,
            &bump_seed,
        ];
        let signer = &[&authority_seeds[..]];
        let target_mint_account = match target_asset {
            MarketAsset::Base => ctx.accounts.base_mint.to_account_info(),
            MarketAsset::Quote => ctx.accounts.quote_mint.to_account_info(),
        };
        let target_decimals = match target_asset {
            MarketAsset::Base => ctx.accounts.base_mint.decimals,
            MarketAsset::Quote => ctx.accounts.quote_mint.decimals,
        };
        if incentive > 0 {
            transfer_checked_with_signer(
                token_program_for_mint(
                    &target_mint_account,
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.custody_target_account.to_account_info(),
                target_mint_account.clone(),
                ctx.accounts.executor_target_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                incentive,
                target_decimals,
                signer,
            )?;
        }
        if owner_amount > 0 {
            transfer_checked_with_signer(
                token_program_for_mint(
                    &target_mint_account,
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.custody_target_account.to_account_info(),
                target_mint_account,
                ctx.accounts.owner_target_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                owner_amount,
                target_decimals,
                signer,
            )?;
        }
        ctx.accounts.order.status = HLP_ORDER_STATUS_EXECUTED;
        Ok(())
    }
}

impl<'info> SettleHlpOrderYield<'info> {
    pub fn handle_settle(
        ctx: Context<'_, '_, '_, 'info, Self>,
        _args: HlpOrderIdArgs,
    ) -> Result<()> {
        validate_hlp_yield_account(
            &ctx.accounts.base_yield_account,
            ctx.accounts.order.key(),
            ctx.accounts.market.key(),
            ctx.accounts.target_hlp_mint.key(),
            ctx.accounts.base_mint.key(),
        )?;
        validate_hlp_yield_account(
            &ctx.accounts.quote_yield_account,
            ctx.accounts.order.key(),
            ctx.accounts.market.key(),
            ctx.accounts.target_hlp_mint.key(),
            ctx.accounts.quote_mint.key(),
        )?;
        require_keys_eq!(
            ctx.accounts.base_yield_account.recipient,
            ctx.accounts.order.owner,
            LeverageDelegateError::InvalidOrder
        );
        require_keys_eq!(
            ctx.accounts.quote_yield_account.recipient,
            ctx.accounts.order.owner,
            LeverageDelegateError::InvalidOrder
        );

        let market_key = ctx.accounts.order.market;
        let owner_key = ctx.accounts.order.owner;
        let target_hlp_mint_key = ctx.accounts.order.target_hlp_mint;
        let order_id_bytes = ctx.accounts.order.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let authority_seeds = &[
            HLP_ORDER_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            target_hlp_mint_key.as_ref(),
            &order_id_bytes,
            &bump_seed,
        ];
        let signer = &[&authority_seeds[..]];

        claim_hlp_yield_if_available(
            ctx.accounts.base_yield_account.accrued_swap_fee_amount,
            ctx.accounts.base_yield_account.accrued_interest_amount,
            ctx.accounts.dusk_program.to_account_info(),
            ctx.accounts.dusk_event_authority.to_account_info(),
            ctx.accounts.market.to_account_info(),
            ctx.accounts.order.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.custody_hlp_account.to_account_info(),
            ctx.accounts.base_reserve_vault.to_account_info(),
            ctx.accounts.base_interest_vault.to_account_info(),
            ctx.accounts.owner_base_account.to_account_info(),
            ctx.accounts.base_yield_account.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            signer,
            ctx.remaining_accounts,
        )?;
        ctx.accounts.quote_yield_account.reload()?;
        claim_hlp_yield_if_available(
            ctx.accounts.quote_yield_account.accrued_swap_fee_amount,
            ctx.accounts.quote_yield_account.accrued_interest_amount,
            ctx.accounts.dusk_program.to_account_info(),
            ctx.accounts.dusk_event_authority.to_account_info(),
            ctx.accounts.market.to_account_info(),
            ctx.accounts.order.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.custody_hlp_account.to_account_info(),
            ctx.accounts.quote_reserve_vault.to_account_info(),
            ctx.accounts.quote_interest_vault.to_account_info(),
            ctx.accounts.owner_quote_account.to_account_info(),
            ctx.accounts.quote_yield_account.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            signer,
            ctx.remaining_accounts,
        )?;
        Ok(())
    }
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

impl<'info> CancelLeverageOrder<'info> {
    pub fn handle_cancel(_ctx: Context<Self>) -> Result<()> {
        Ok(())
    }
}

impl<'info> BeforeLeverageOrder<'info> {
    pub fn handle_before(
        ctx: Context<Self>,
        _args: ExecuteOrderArgs,
        expected_kind: u8,
    ) -> Result<()> {
        let order = &mut ctx.accounts.order;
        require!(
            order.kind == expected_kind,
            LeverageDelegateError::InvalidOrder
        );
        let clock = Clock::get()?;
        let current_slot = clock.slot;
        let closeout_value = ctx.accounts.market.leverage_closeout_value_at_time(
            &ctx.accounts.leverage_position,
            current_slot,
            clock.unix_timestamp,
        )?;
        let closeout_price_nad: u64 = (closeout_value as u128)
            .checked_mul(NAD as u128)
            .ok_or(LeverageDelegateError::MathOverflow)?
            .checked_div(ctx.accounts.leverage_position.collateral_amount as u128)
            .ok_or(LeverageDelegateError::MathOverflow)?
            .try_into()
            .map_err(|_| LeverageDelegateError::MathOverflow)?;
        match expected_kind {
            ORDER_KIND_TAKE_PROFIT => require!(
                closeout_price_nad >= order.trigger_closeout_price_nad,
                LeverageDelegateError::TriggerNotMet
            ),
            ORDER_KIND_STOP_LOSS => require!(
                closeout_price_nad <= order.trigger_closeout_price_nad,
                LeverageDelegateError::TriggerNotMet
            ),
            _ => return err!(LeverageDelegateError::InvalidOrder),
        }
        let debt_asset = ctx.accounts.leverage_position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint = ctx.accounts.market.side(debt_asset).asset_mint;
        let collateral_mint = ctx.accounts.market.side(collateral_asset).asset_mint;
        require_keys_eq!(
            ctx.accounts.token_mint.key(),
            debt_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.collateral_mint.key(),
            collateral_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require!(
            ctx.accounts.custody_token_account.amount == 0,
            LeverageDelegateError::InvalidTokenAccount
        );
        let close_slice = ctx
            .accounts
            .market
            .leverage_close_slice(&ctx.accounts.leverage_position, order.close_bps)?;
        let collateral_credit = close_slice
            .collateral_amount
            .checked_sub(get_transfer_fee(
                &ctx.accounts.collateral_mint.to_account_info(),
                close_slice.collateral_amount,
            )?)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        let close_quote = ctx.accounts.market.quote_leverage_swap_at_time(
            collateral_asset,
            collateral_credit,
            current_slot,
            clock.unix_timestamp,
        )?;
        let cash_repaid = ctx
            .accounts
            .market
            .debt
            .isolated_repayment_for_max(debt_asset, close_slice.debt_shares, u64::MAX)?
            .cash_repaid;
        let residual = close_quote
            .amount_out
            .checked_sub(cash_repaid)
            .ok_or(LeverageDelegateError::InvalidOrder)?;
        let output_amount = residual
            .checked_sub(get_transfer_fee(
                &ctx.accounts.token_mint.to_account_info(),
                residual,
            )?)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        order.staged_margin = if order.close_bps == BPS_DENOMINATOR {
            ctx.accounts.leverage_position.margin_amount
        } else {
            output_amount
        };
        order.staged_collateral_amount = close_slice.collateral_amount;
        order.staged_remaining_collateral_amount = ctx
            .accounts
            .leverage_position
            .collateral_amount
            .checked_sub(close_slice.collateral_amount)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        order.staged_remaining_debt_shares = ctx
            .accounts
            .leverage_position
            .debt_shares
            .checked_sub(close_slice.debt_shares)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        order.staged_remaining_debt_principal = ctx
            .accounts
            .leverage_position
            .debt_principal
            .checked_sub(close_slice.debt_principal)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        order.staged_custody_token_account = ctx.accounts.custody_token_account.key();
        order.staged_output_mint = ctx.accounts.token_mint.key();
        order.staged_output_amount = output_amount;
        let approval = LeverageDelegationApproval::new(
            LEVERAGE_DELEGATE_CLOSE,
            ctx.accounts.market.key(),
            order.owner,
            ctx.accounts.leverage_position.key(),
            ctx.accounts.leverage_delegation.key(),
            debt_asset,
            ctx.accounts.custody_token_account.key(),
            ctx.accounts.token_mint.key(),
            close_slice.collateral_amount,
            output_amount,
        );
        let mut data = Vec::new();
        approval
            .serialize(&mut data)
            .map_err(|_| LeverageDelegateError::ApprovalSerializationFailed)?;
        set_return_data(&data);
        Ok(())
    }
}

impl<'info> AfterCloseOrder<'info> {
    pub fn handle_after(ctx: Context<Self>, _args: ExecuteOrderArgs) -> Result<()> {
        require_eq!(
            ctx.accounts.leverage_position.debt_shares,
            ctx.accounts.order.staged_remaining_debt_shares,
            LeverageDelegateError::InvalidOrder
        );
        require_eq!(
            ctx.accounts.leverage_position.debt_principal,
            ctx.accounts.order.staged_remaining_debt_principal,
            LeverageDelegateError::InvalidOrder
        );
        require_eq!(
            ctx.accounts.leverage_position.collateral_amount,
            ctx.accounts.order.staged_remaining_collateral_amount,
            LeverageDelegateError::InvalidOrder
        );
        require_keys_eq!(
            ctx.accounts.order.staged_custody_token_account,
            ctx.accounts.custody_token_account.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.order.staged_output_mint,
            ctx.accounts.token_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        require!(
            ctx.accounts.order.staged_output_amount == ctx.accounts.custody_token_account.amount,
            LeverageDelegateError::InvalidTokenAccount
        );

        let order_market = ctx.accounts.order.market;
        let order_owner = ctx.accounts.order.owner;
        let order_position = ctx.accounts.order.position;
        let order_id_bytes = ctx.accounts.order.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let staged_margin = ctx.accounts.order.staged_margin;
        let staged_output_amount = ctx.accounts.order.staged_output_amount;
        let custody_token_account_key = ctx.accounts.custody_token_account.key();
        let token_mint_key = ctx.accounts.token_mint.key();
        let delegation_key = ctx.accounts.leverage_delegation.key();
        let debt_asset = ctx.accounts.leverage_delegation.debt_asset()?;
        let amount = ctx.accounts.custody_token_account.amount;

        if amount > 0 {
            let incentive = min(
                amount,
                ceil_div(
                    (staged_margin as u128)
                        .checked_mul(EXECUTOR_INCENTIVE_BPS as u128)
                        .ok_or(LeverageDelegateError::MathOverflow)?,
                    BPS_DENOMINATOR as u128,
                )
                .ok_or(LeverageDelegateError::MathOverflow)? as u64,
            );
            let owner_amount = amount
                .checked_sub(incentive)
                .ok_or(LeverageDelegateError::MathOverflow)?;
            let signer_seeds = &[
                ORDER_SEED_PREFIX,
                order_position.as_ref(),
                order_owner.as_ref(),
                &order_id_bytes,
                &bump_seed,
            ];
            let signer = &[&signer_seeds[..]];

            if incentive > 0 {
                transfer_checked_with_signer(
                    token_program_for_mint(
                        &ctx.accounts.token_mint.to_account_info(),
                        &ctx.accounts.token_program.to_account_info(),
                        &ctx.accounts.token_2022_program.to_account_info(),
                    ),
                    ctx.accounts.custody_token_account.to_account_info(),
                    ctx.accounts.token_mint.to_account_info(),
                    ctx.accounts.executor_token_account.to_account_info(),
                    ctx.accounts.order.to_account_info(),
                    incentive,
                    ctx.accounts.token_mint.decimals,
                    signer,
                )?;
            }
            if owner_amount > 0 {
                transfer_checked_with_signer(
                    token_program_for_mint(
                        &ctx.accounts.token_mint.to_account_info(),
                        &ctx.accounts.token_program.to_account_info(),
                        &ctx.accounts.token_2022_program.to_account_info(),
                    ),
                    ctx.accounts.custody_token_account.to_account_info(),
                    ctx.accounts.token_mint.to_account_info(),
                    ctx.accounts.owner_token_account.to_account_info(),
                    ctx.accounts.order.to_account_info(),
                    owner_amount,
                    ctx.accounts.token_mint.decimals,
                    signer,
                )?;
            }
        }

        let approval = LeverageDelegationApproval::new(
            LEVERAGE_DELEGATE_CLOSE_SETTLED,
            order_market,
            order_owner,
            order_position,
            delegation_key,
            debt_asset,
            custody_token_account_key,
            token_mint_key,
            ctx.accounts.order.staged_collateral_amount,
            staged_output_amount,
        );
        let mut data = Vec::new();
        approval
            .serialize(&mut data)
            .map_err(|_| LeverageDelegateError::ApprovalSerializationFailed)?;
        set_return_data(&data);
        Ok(())
    }
}

fn reset_staged_settlement(order: &mut LeverageOrder) {
    order.staged_margin = 0;
    order.staged_collateral_amount = 0;
    order.staged_remaining_collateral_amount = 0;
    order.staged_remaining_debt_shares = 0;
    order.staged_remaining_debt_principal = 0;
    order.staged_custody_token_account = Pubkey::default();
    order.staged_output_mint = Pubkey::default();
    order.staged_output_amount = 0;
}

#[inline(never)]
fn preview_hlp_order_trigger<'info>(
    accounts: &ExecuteHlpOrder<'info>,
    target_asset: MarketAsset,
    hlp_amount: u64,
) -> Result<dusk::instructions::HlpOrderTriggerPreview> {
    Ok(dusk::cpi::preview_hlp_order_trigger(
        CpiContext::new(
            accounts.dusk_program.to_account_info(),
            dusk::cpi::accounts::PreviewHlpOrderTrigger {
                market: accounts.market.to_account_info(),
            },
        ),
        dusk::instructions::PreviewHlpOrderTriggerArgs {
            target_asset: target_asset.code(),
            hlp_amount,
        },
    )?
    .get())
}

#[inline(never)]
fn withdraw_hlp_order_position<'info>(
    accounts: &ExecuteHlpOrder<'info>,
    remaining_accounts: &[AccountInfo<'info>],
) -> Result<()> {
    let market_key = accounts.order.market;
    let owner_key = accounts.order.owner;
    let target_hlp_mint_key = accounts.order.target_hlp_mint;
    let order_id_bytes = accounts.order.order_id.to_le_bytes();
    let bump_seed = [accounts.order.bump];
    let authority_seeds = &[
        HLP_ORDER_SEED_PREFIX,
        market_key.as_ref(),
        owner_key.as_ref(),
        target_hlp_mint_key.as_ref(),
        &order_id_bytes,
        &bump_seed,
    ];
    dusk::cpi::withdraw_single_sided(
        CpiContext::new_with_signer(
            accounts.dusk_program.to_account_info(),
            dusk::cpi::accounts::WithdrawSingleSided {
                market: accounts.market.to_account_info(),
                futarchy_authority: accounts.futarchy_authority.to_account_info(),
                owner: accounts.order.to_account_info(),
                base_mint: accounts.base_mint.to_account_info(),
                quote_mint: accounts.quote_mint.to_account_info(),
                ylp_mint: accounts.ylp_mint.to_account_info(),
                target_hlp_mint: accounts.target_hlp_mint.to_account_info(),
                base_reserve_vault: accounts.base_reserve_vault.to_account_info(),
                quote_reserve_vault: accounts.quote_reserve_vault.to_account_info(),
                borrowed_interest_vault: accounts.borrowed_interest_vault.to_account_info(),
                owner_target_account: accounts.custody_target_account.to_account_info(),
                owner_hlp_account: accounts.custody_hlp_account.to_account_info(),
                hlp_ylp_account: accounts.hlp_ylp_account.to_account_info(),
                base_yield_account: accounts.base_yield_account.to_account_info(),
                quote_yield_account: accounts.quote_yield_account.to_account_info(),
                token_program: accounts.token_program.to_account_info(),
                token_2022_program: accounts.token_2022_program.to_account_info(),
                event_authority: accounts.dusk_event_authority.to_account_info(),
                program: accounts.dusk_program.to_account_info(),
            },
            &[&authority_seeds[..]],
        )
        .with_remaining_accounts(remaining_accounts.to_vec()),
        WithdrawSingleSidedArgs {
            hlp_amount: accounts.order.hlp_amount,
            min_target_amount_out: accounts.order.min_target_amount_out,
        },
    )
}

fn validate_hlp_yield_account(
    account: &YieldAccount,
    owner: Pubkey,
    market: Pubkey,
    lp_mint: Pubkey,
    asset_mint: Pubkey,
) -> Result<()> {
    account.assert_account(owner, market, lp_mint, asset_mint, YieldTokenKind::Hlp)
}

#[allow(clippy::too_many_arguments)]
fn set_hlp_yield_recipient<'info>(
    dusk_program: AccountInfo<'info>,
    event_authority: AccountInfo<'info>,
    market: AccountInfo<'info>,
    owner: AccountInfo<'info>,
    lp_mint: AccountInfo<'info>,
    asset_mint: AccountInfo<'info>,
    yield_account: AccountInfo<'info>,
    recipient: Pubkey,
    signer: &[&[&[u8]]],
) -> Result<()> {
    dusk::cpi::set_yield_recipient(
        CpiContext::new_with_signer(
            dusk_program.clone(),
            dusk::cpi::accounts::SetYieldRecipient {
                market,
                owner,
                asset_mint,
                lp_mint,
                yield_account,
                event_authority,
                program: dusk_program,
            },
            signer,
        ),
        SetYieldRecipientArgs {
            token_kind: YieldTokenKind::Hlp,
            recipient,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn claim_hlp_yield_if_available<'info>(
    accrued_swap_fee_amount: u64,
    accrued_interest_amount: u64,
    dusk_program: AccountInfo<'info>,
    event_authority: AccountInfo<'info>,
    market: AccountInfo<'info>,
    owner: AccountInfo<'info>,
    asset_mint: AccountInfo<'info>,
    lp_mint: AccountInfo<'info>,
    owner_lp_account: AccountInfo<'info>,
    reserve_vault: AccountInfo<'info>,
    interest_vault: AccountInfo<'info>,
    recipient_asset_account: AccountInfo<'info>,
    yield_account: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    token_2022_program: AccountInfo<'info>,
    signer: &[&[&[u8]]],
    remaining_accounts: &[AccountInfo<'info>],
) -> Result<()> {
    if accrued_swap_fee_amount == 0 && accrued_interest_amount == 0 {
        return Ok(());
    }
    dusk::cpi::claim_yield(
        CpiContext::new_with_signer(
            dusk_program.clone(),
            dusk::cpi::accounts::ClaimYield {
                market,
                owner,
                asset_mint,
                lp_mint,
                owner_lp_account,
                reserve_vault,
                interest_vault,
                recipient_asset_account,
                yield_account,
                token_program,
                token_2022_program,
                event_authority,
                program: dusk_program,
            },
            signer,
        )
        .with_remaining_accounts(remaining_accounts.to_vec()),
        ClaimYieldArgs {
            token_kind: YieldTokenKind::Hlp,
        },
    )
}

fn token_program_for_mint<'info>(
    mint: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    token_2022_program: &AccountInfo<'info>,
) -> AccountInfo<'info> {
    if mint.owner == token_program.key {
        token_program.clone()
    } else {
        token_2022_program.clone()
    }
}

fn transfer_checked_with_signer<'info>(
    token_program: AccountInfo<'info>,
    from: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if *token_program.key == Token2022::id() {
        token_2022::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                token_2022::TransferChecked {
                    from,
                    mint,
                    to,
                    authority,
                },
                signer_seeds,
            ),
            amount,
            decimals,
        )
    } else {
        token::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                token::TransferChecked {
                    from,
                    mint,
                    to,
                    authority,
                },
                signer_seeds,
            ),
            amount,
            decimals,
        )
    }
}

fn validate_order_kind(kind: u8) -> Result<()> {
    require!(
        kind == ORDER_KIND_TAKE_PROFIT || kind == ORDER_KIND_STOP_LOSS,
        LeverageDelegateError::InvalidOrder
    );
    Ok(())
}

fn validate_hlp_order_kind(kind: u8) -> Result<()> {
    require!(
        kind == HLP_ORDER_KIND_STOP_LOSS || kind == HLP_ORDER_KIND_STOP_RATE,
        LeverageDelegateError::InvalidOrder
    );
    Ok(())
}

fn hlp_order_trigger_met(
    kind: u8,
    principal_nav_nad: u64,
    funding_apr_nad: u128,
    trigger_nad: u64,
) -> Result<bool> {
    match kind {
        HLP_ORDER_KIND_STOP_LOSS => Ok(principal_nav_nad <= trigger_nad),
        HLP_ORDER_KIND_STOP_RATE => Ok(funding_apr_nad >= trigger_nad as u128),
        _ => err!(LeverageDelegateError::InvalidOrder),
    }
}

#[error_code]
pub enum LeverageDelegateError {
    #[msg("Invalid leverage order")]
    InvalidOrder,
    #[msg("Order trigger is not met")]
    TriggerNotMet,
    #[msg("Invalid token account")]
    InvalidTokenAccount,
    #[msg("Math overflow")]
    MathOverflow,
    #[msg("Approval serialization failed")]
    ApprovalSerializationFailed,
    #[msg("Unsupported Dusk market version")]
    InvalidMarketVersion,
}

#[cfg(test)]
mod tests {
    include!("tests/mod.rs");
}
