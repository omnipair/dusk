use anchor_lang::{prelude::*, solana_program::program::set_return_data};
use anchor_spl::{
    token::{self, Token},
    token_2022::{self, Token2022},
    token_interface::{Mint, TokenAccount},
};
use dusk::{
    constants::{BPS_DENOMINATOR, MARKET_LAYOUT_VERSION, NAD},
    instructions::{
        LeverageDelegationApproval, LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_CLOSE_SETTLED,
    },
    shared::{math::ceil_div, token::get_transfer_fee},
    state::{LeverageDelegation, LeveragePosition, Market},
};
use std::cmp::min;

declare_id!("EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp");

pub const ORDER_SEED_PREFIX: &[u8] = b"leverage_order";
pub const CUSTODY_AUTHORITY_SEED_PREFIX: &[u8] = b"leverage_delegate_authority";
pub const EXECUTOR_INCENTIVE_BPS: u64 = 500;
pub const ORDER_KIND_TAKE_PROFIT: u8 = 1;
pub const ORDER_KIND_STOP_LOSS: u8 = 2;

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
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateLeverageOrderArgs {
    pub order_id: u64,
    pub kind: u8,
    pub trigger_closeout_price_nad: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateLeverageOrderArgs {
    pub order_id: u64,
    pub kind: u8,
    pub trigger_closeout_price_nad: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CancelLeverageOrderArgs {
    pub order_id: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ExecuteOrderArgs {
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
    pub staged_margin: u64,
    pub staged_custody_token_account: Pubkey,
    pub staged_output_mint: Pubkey,
    pub staged_output_amount: u64,
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
    /// CHECK: PDA authority for the custody token account approved as close recipient.
    #[account(
        seeds = [CUSTODY_AUTHORITY_SEED_PREFIX, order.key().as_ref()],
        bump
    )]
    pub custody_authority: AccountInfo<'info>,
    #[account(
        constraint = custody_token_account.owner == custody_authority.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
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
    /// CHECK: PDA authority for the custody token account.
    #[account(
        seeds = [CUSTODY_AUTHORITY_SEED_PREFIX, order.key().as_ref()],
        bump
    )]
    pub custody_authority: AccountInfo<'info>,
    #[account(
        mut,
        constraint = custody_token_account.key() == order.staged_custody_token_account @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_token_account.owner == custody_authority.key() @ LeverageDelegateError::InvalidTokenAccount,
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

impl<'info> CreateLeverageOrder<'info> {
    pub fn handle_create(ctx: Context<Self>, args: CreateLeverageOrderArgs) -> Result<()> {
        validate_order_kind(args.kind)?;
        require!(
            args.trigger_closeout_price_nad > 0,
            LeverageDelegateError::InvalidOrder
        );
        let order = &mut ctx.accounts.order;
        order.owner = ctx.accounts.owner.key();
        order.market = ctx.accounts.market.key();
        order.position = ctx.accounts.leverage_position.key();
        order.order_id = args.order_id;
        order.kind = args.kind;
        order.trigger_closeout_price_nad = args.trigger_closeout_price_nad;
        reset_staged_settlement(order);
        order.bump = ctx.bumps.order;
        Ok(())
    }
}

impl<'info> UpdateLeverageOrder<'info> {
    pub fn handle_update(ctx: Context<Self>, args: UpdateLeverageOrderArgs) -> Result<()> {
        validate_order_kind(args.kind)?;
        require!(
            args.trigger_closeout_price_nad > 0,
            LeverageDelegateError::InvalidOrder
        );
        let order = &mut ctx.accounts.order;
        order.kind = args.kind;
        order.trigger_closeout_price_nad = args.trigger_closeout_price_nad;
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
        let current_slot = Clock::get()?.slot;
        let closeout_value = ctx
            .accounts
            .market
            .leverage_closeout_value(&ctx.accounts.leverage_position, current_slot)?;
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
        let debt_mint = ctx.accounts.market.side(debt_asset).asset_mint;
        require_keys_eq!(
            ctx.accounts.token_mint.key(),
            debt_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require!(
            ctx.accounts.custody_token_account.amount == 0,
            LeverageDelegateError::InvalidTokenAccount
        );
        let debt_amount = ctx
            .accounts
            .leverage_position
            .debt_amount(&ctx.accounts.market.debt)?;
        let residual = closeout_value
            .checked_sub(debt_amount)
            .ok_or(LeverageDelegateError::InvalidOrder)?;
        let output_amount = residual
            .checked_sub(get_transfer_fee(
                &ctx.accounts.token_mint.to_account_info(),
                residual,
            )?)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        order.staged_margin = ctx.accounts.leverage_position.margin_amount;
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
        require!(
            ctx.accounts.leverage_position.debt_shares == 0
                && ctx.accounts.leverage_position.collateral_amount == 0,
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

        let order_key = ctx.accounts.order.key();
        let order_market = ctx.accounts.order.market;
        let order_owner = ctx.accounts.order.owner;
        let order_position = ctx.accounts.order.position;
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
            let bump = ctx.bumps.custody_authority;
            let signer_seeds = &[CUSTODY_AUTHORITY_SEED_PREFIX, order_key.as_ref(), &[bump]];
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
                    ctx.accounts.custody_authority.to_account_info(),
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
                    ctx.accounts.custody_authority.to_account_info(),
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
    order.staged_custody_token_account = Pubkey::default();
    order.staged_output_mint = Pubkey::default();
    order.staged_output_amount = 0;
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
