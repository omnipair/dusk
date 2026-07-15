use anchor_lang::{prelude::*, solana_program::program::set_return_data};
use anchor_spl::{
    token::{self, Token},
    token_2022::{self, Token2022},
    token_interface::{Mint, TokenAccount},
};
use dusk::{
    constants::{BPS_DENOMINATOR, NAD},
    instructions::{
        LeverageDelegationApproval, LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_CLOSE_SETTLED,
    },
    shared::{
        math::ceil_div,
        token::{get_transfer_fee, get_transfer_inverse_fee},
    },
    state::{LeverageDelegation, LeverageMarginMode, LeveragePosition, Market, MarketAsset},
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
    pub staged_incentive_basis: u64,
    pub staged_custody_token_account: Pubkey,
    pub staged_output_mint: Pubkey,
    pub staged_output_amount: u64,
    pub bump: u8,
}

#[derive(Accounts)]
#[instruction(args: CreateLeverageOrderArgs)]
pub struct CreateLeverageOrder<'info> {
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
    pub collateral_mint: Option<Box<InterfaceAccount<'info, Mint>>>,
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
        let collateral_asset = ctx.accounts.leverage_position.collateral_asset()?;
        let expected_collateral_mint = ctx.accounts.market.side(collateral_asset)?.asset_mint;
        let collateral_mint_info = if ctx.accounts.token_mint.key() == expected_collateral_mint {
            ctx.accounts.token_mint.to_account_info()
        } else {
            let collateral_mint = ctx
                .accounts
                .collateral_mint
                .as_ref()
                .ok_or(LeverageDelegateError::InvalidTokenAccount)?;
            require_keys_eq!(
                collateral_mint.key(),
                expected_collateral_mint,
                LeverageDelegateError::InvalidTokenAccount
            );
            collateral_mint.to_account_info()
        };
        let collateral_swap_input = transfer_net_amount(
            &collateral_mint_info,
            ctx.accounts.leverage_position.collateral_amount,
        )?;
        let closeout_price_nad = closeout_price_per_unit_nad(
            &ctx.accounts.market,
            &ctx.accounts.leverage_position,
            collateral_swap_input,
        )?;
        require_trigger_met(
            expected_kind,
            closeout_price_nad,
            order.trigger_closeout_price_nad,
        )?;
        let debt_asset = ctx.accounts.leverage_position.debt_asset()?;
        let settlement_plan = close_settlement_plan(
            &ctx.accounts.market,
            &ctx.accounts.leverage_position,
            collateral_swap_input,
        )?;
        let settlement_mint = ctx
            .accounts
            .market
            .side(settlement_plan.settlement_asset)?
            .asset_mint;
        require_keys_eq!(
            ctx.accounts.token_mint.key(),
            settlement_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require!(
            ctx.accounts.custody_token_account.amount == 0,
            LeverageDelegateError::InvalidTokenAccount
        );
        let input_transfer_fee = settlement_plan
            .collateral_swap_input
            .map(|amount| {
                get_transfer_inverse_fee(&ctx.accounts.token_mint.to_account_info(), amount)
            })
            .transpose()?
            .unwrap_or(0);
        let residual = settlement_plan
            .gross_residual_before_input_transfer_fee
            .checked_sub(input_transfer_fee)
            .ok_or(LeverageDelegateError::InvalidOrder)?;
        let output_amount =
            transfer_net_amount(&ctx.accounts.token_mint.to_account_info(), residual)?;
        stage_close_settlement(
            order,
            output_amount,
            ctx.accounts.custody_token_account.key(),
            ctx.accounts.token_mint.key(),
            output_amount,
        );
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
        require_closed_leverage_position(&ctx.accounts.leverage_position)?;
        require_staged_settlement(
            &ctx.accounts.order,
            ctx.accounts.custody_token_account.key(),
            ctx.accounts.token_mint.key(),
            ctx.accounts.custody_token_account.amount,
        )?;

        let order_key = ctx.accounts.order.key();
        let order_market = ctx.accounts.order.market;
        let order_owner = ctx.accounts.order.owner;
        let order_position = ctx.accounts.order.position;
        let staged_incentive_basis = ctx.accounts.order.staged_incentive_basis;
        let staged_output_amount = ctx.accounts.order.staged_output_amount;
        let custody_token_account_key = ctx.accounts.custody_token_account.key();
        let token_mint_key = ctx.accounts.token_mint.key();
        let delegation_key = ctx.accounts.leverage_delegation.key();
        let debt_asset = ctx.accounts.leverage_delegation.debt_asset()?;
        let amount = ctx.accounts.custody_token_account.amount;

        if amount > 0 {
            let incentive = executor_incentive(amount, staged_incentive_basis)?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CloseSettlementPlan {
    settlement_asset: MarketAsset,
    gross_residual_before_input_transfer_fee: u64,
    collateral_swap_input: Option<u64>,
}

fn close_settlement_plan(
    market: &Market,
    position: &LeveragePosition,
    collateral_swap_input: u64,
) -> Result<CloseSettlementPlan> {
    let debt_amount = position.debt_amount(&market.debt)?;
    let settlement_asset = position.settlement_asset()?;
    match position.margin_mode()? {
        LeverageMarginMode::Debt => Ok(CloseSettlementPlan {
            settlement_asset,
            gross_residual_before_input_transfer_fee: market
                .quote_leverage_swap(position.collateral_asset()?, collateral_swap_input)?
                .amount_out
                .checked_sub(debt_amount)
                .ok_or(LeverageDelegateError::InvalidOrder)?,
            collateral_swap_input: None,
        }),
        LeverageMarginMode::Collateral => {
            let swap = market.quote_leverage_swap_exact_output(settlement_asset, debt_amount)?;
            Ok(CloseSettlementPlan {
                settlement_asset,
                gross_residual_before_input_transfer_fee: position
                    .collateral_amount
                    .checked_sub(swap.amount_in)
                    .ok_or(LeverageDelegateError::InvalidOrder)?,
                collateral_swap_input: Some(swap.amount_in),
            })
        }
    }
}

fn reset_staged_settlement(order: &mut LeverageOrder) {
    order.staged_incentive_basis = 0;
    order.staged_custody_token_account = Pubkey::default();
    order.staged_output_mint = Pubkey::default();
    order.staged_output_amount = 0;
}

fn stage_close_settlement(
    order: &mut LeverageOrder,
    incentive_basis: u64,
    custody_token_account: Pubkey,
    output_mint: Pubkey,
    output_amount: u64,
) {
    order.staged_incentive_basis = incentive_basis;
    order.staged_custody_token_account = custody_token_account;
    order.staged_output_mint = output_mint;
    order.staged_output_amount = output_amount;
}

fn require_staged_settlement(
    order: &LeverageOrder,
    custody_token_account: Pubkey,
    output_mint: Pubkey,
    output_amount: u64,
) -> Result<()> {
    require_keys_eq!(
        order.staged_custody_token_account,
        custody_token_account,
        LeverageDelegateError::InvalidTokenAccount
    );
    require_keys_eq!(
        order.staged_output_mint,
        output_mint,
        LeverageDelegateError::InvalidTokenAccount
    );
    require!(
        order.staged_output_amount == output_amount,
        LeverageDelegateError::InvalidTokenAccount
    );
    Ok(())
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

fn transfer_net_amount(mint: &AccountInfo, gross_amount: u64) -> Result<u64> {
    let fee = get_transfer_fee(mint, gross_amount)?;
    gross_amount
        .checked_sub(fee)
        .ok_or(LeverageDelegateError::MathOverflow.into())
}

fn validate_order_kind(kind: u8) -> Result<()> {
    require!(
        kind == ORDER_KIND_TAKE_PROFIT || kind == ORDER_KIND_STOP_LOSS,
        LeverageDelegateError::InvalidOrder
    );
    Ok(())
}

fn require_trigger_met(
    kind: u8,
    closeout_price_nad: u64,
    trigger_closeout_price_nad: u64,
) -> Result<()> {
    match kind {
        ORDER_KIND_TAKE_PROFIT => require!(
            closeout_price_nad >= trigger_closeout_price_nad,
            LeverageDelegateError::TriggerNotMet
        ),
        ORDER_KIND_STOP_LOSS => require!(
            closeout_price_nad <= trigger_closeout_price_nad,
            LeverageDelegateError::TriggerNotMet
        ),
        _ => return err!(LeverageDelegateError::InvalidOrder),
    }
    Ok(())
}

fn executor_incentive(amount: u64, settlement_basis: u64) -> Result<u64> {
    Ok(min(
        amount,
        ceil_div(
            (settlement_basis as u128)
                .checked_mul(EXECUTOR_INCENTIVE_BPS as u128)
                .ok_or(LeverageDelegateError::MathOverflow)?,
            BPS_DENOMINATOR as u128,
        )
        .ok_or(LeverageDelegateError::MathOverflow)? as u64,
    ))
}

fn require_closed_leverage_position(position: &LeveragePosition) -> Result<()> {
    require!(
        position.debt_shares == 0 && position.collateral_amount == 0,
        LeverageDelegateError::InvalidOrder
    );
    Ok(())
}

fn closeout_price_per_unit_nad(
    market: &Market,
    position: &LeveragePosition,
    collateral_swap_input: u64,
) -> Result<u64> {
    let closeout_value = market
        .quote_leverage_swap(position.collateral_asset()?, collateral_swap_input)?
        .amount_out;
    Ok((closeout_value as u128)
        .checked_mul(NAD as u128)
        .ok_or(LeverageDelegateError::MathOverflow)?
        .checked_div(position.collateral_amount as u128)
        .ok_or(LeverageDelegateError::MathOverflow)?
        .try_into()
        .map_err(|_| LeverageDelegateError::MathOverflow)?)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use dusk::state::{
        Debt, HlpVault, Insurance, MarketConfig, MarketSide, PendingAuthorityChange,
        PendingConfigChange, ReserveShares, Reserves, Risk,
    };

    fn test_market(base_cash: u64, quote_cash: u64) -> Market {
        let mut base_side = MarketSide::default();
        base_side.reserves = Reserves {
            live_reserve: base_cash,
            cash_reserve: base_cash,
            reserved_liability: 0,
        };
        base_side.shares = ReserveShares {
            ylp_supply: base_cash,
        };
        let mut quote_side = MarketSide::default();
        quote_side.reserves = Reserves {
            live_reserve: quote_cash,
            cash_reserve: quote_cash,
            reserved_liability: 0,
        };
        quote_side.shares = ReserveShares {
            ylp_supply: quote_cash,
        };
        Market {
            version: 2,
            base_mint: Pubkey::new_unique(),
            quote_mint: Pubkey::new_unique(),
            ylp_mint: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            manager: Pubkey::new_unique(),
            base_side,
            quote_side,
            config: MarketConfig {
                swap_fee_bps: 0,
                ..MarketConfig::default()
            },
            debt: Debt {
                base_borrow_index_nad: NAD as u128,
                quote_borrow_index_nad: NAD as u128,
                base_rate_at_target_nad: dusk::constants::INTEREST_INITIAL_RATE_AT_TARGET_NAD,
                quote_rate_at_target_nad: dusk::constants::INTEREST_INITIAL_RATE_AT_TARGET_NAD,
                ..Debt::default()
            },
            base_hlp_vault: HlpVault::default(),
            quote_hlp_vault: HlpVault::default(),
            risk: Risk::default(),
            insurance: Insurance::default(),
            pending_config: PendingConfigChange::default(),
            pending_operator: PendingAuthorityChange::default(),
            pending_manager: PendingAuthorityChange::default(),
            params_hash: [0u8; 32],
            last_update_slot: 0,
            reduce_only: false,
            bump: 255,
        }
    }

    fn leverage_order() -> LeverageOrder {
        LeverageOrder {
            owner: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            position: Pubkey::new_unique(),
            order_id: 1,
            kind: ORDER_KIND_TAKE_PROFIT,
            trigger_closeout_price_nad: NAD,
            staged_incentive_basis: 0,
            staged_custody_token_account: Pubkey::default(),
            staged_output_mint: Pubkey::default(),
            staged_output_amount: 0,
            bump: 255,
        }
    }

    fn leverage_position(margin_mode: LeverageMarginMode) -> LeveragePosition {
        LeveragePosition {
            owner: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            position_id: Pubkey::new_unique(),
            debt_asset: dusk::state::MarketAsset::Base.code(),
            margin_mode: margin_mode.code(),
            collateral_amount: 2_000,
            margin_amount: 1_000,
            open_notional: 2_000,
            debt_principal: 1_000,
            debt_shares: 1_000,
            multiplier_bps: 20_000,
            opened_at: 0,
            opened_slot: 0,
            bump: 255,
        }
    }

    #[test]
    fn settlement_asset_selection_supports_both_margin_modes() {
        let debt_margin = leverage_position(LeverageMarginMode::Debt);
        assert_eq!(
            debt_margin.settlement_asset().unwrap(),
            dusk::state::MarketAsset::Base
        );

        let collateral_margin = leverage_position(LeverageMarginMode::Collateral);
        assert_eq!(
            collateral_margin.settlement_asset().unwrap(),
            dusk::state::MarketAsset::Quote
        );
    }

    #[test]
    fn close_settlement_plans_match_each_margin_mode() {
        let market = test_market(1_000_000, 1_000_000);

        let debt_margin = leverage_position(LeverageMarginMode::Debt);
        let collateral_swap_input = debt_margin.collateral_amount - 10;
        let debt_plan =
            close_settlement_plan(&market, &debt_margin, collateral_swap_input).unwrap();
        let debt_amount = debt_margin.debt_amount(&market.debt).unwrap();
        assert_eq!(debt_plan.settlement_asset, MarketAsset::Base);
        assert_eq!(debt_plan.collateral_swap_input, None);
        assert_eq!(
            debt_plan.gross_residual_before_input_transfer_fee,
            market
                .quote_leverage_swap(MarketAsset::Quote, collateral_swap_input)
                .unwrap()
                .amount_out
                - debt_amount
        );

        let collateral_margin = leverage_position(LeverageMarginMode::Collateral);
        let collateral_plan = close_settlement_plan(
            &market,
            &collateral_margin,
            collateral_margin.collateral_amount,
        )
        .unwrap();
        let collateral_swap = market
            .quote_leverage_swap_exact_output(MarketAsset::Quote, debt_amount)
            .unwrap();
        assert_eq!(collateral_plan.settlement_asset, MarketAsset::Quote);
        assert_eq!(
            collateral_plan.collateral_swap_input,
            Some(collateral_swap.amount_in)
        );
        assert_eq!(
            collateral_plan.gross_residual_before_input_transfer_fee,
            collateral_margin.collateral_amount - collateral_swap.amount_in
        );
    }

    #[test]
    fn order_kind_validation_accepts_only_tp_or_sl() {
        assert!(validate_order_kind(ORDER_KIND_TAKE_PROFIT).is_ok());
        assert!(validate_order_kind(ORDER_KIND_STOP_LOSS).is_ok());
        assert!(validate_order_kind(0).is_err());
    }

    #[test]
    fn executor_incentive_uses_settlement_denominated_basis() {
        assert_eq!(executor_incentive(1_000, 1_000).unwrap(), 50);
        assert_eq!(executor_incentive(3, 10_000).unwrap(), 3);
    }

    #[test]
    fn executor_incentive_rounds_up() {
        assert_eq!(executor_incentive(10, 1).unwrap(), 1);
    }

    #[test]
    fn staged_settlement_defaults_reject_direct_after_close_cleanup() {
        let order = leverage_order();
        let custody = Pubkey::new_unique();
        let mint = Pubkey::new_unique();

        assert!(require_staged_settlement(&order, custody, mint, 0).is_err());
    }

    #[test]
    fn stage_close_settlement_binds_custody_mint_and_amount() {
        let mut order = leverage_order();
        let custody = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        stage_close_settlement(&mut order, 10_000, custody, mint, 123);

        assert_eq!(order.staged_incentive_basis, 10_000);
        assert_eq!(order.staged_custody_token_account, custody);
        assert_eq!(order.staged_output_mint, mint);
        assert_eq!(order.staged_output_amount, 123);
        assert!(require_staged_settlement(&order, custody, mint, 123).is_ok());
    }

    #[test]
    fn staged_settlement_rejects_wrong_custody_mint_or_amount() {
        let mut order = leverage_order();
        let custody = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        stage_close_settlement(&mut order, 10_000, custody, mint, 123);

        assert!(require_staged_settlement(&order, Pubkey::new_unique(), mint, 123).is_err());
        assert!(require_staged_settlement(&order, custody, Pubkey::new_unique(), 123).is_err());
        assert!(require_staged_settlement(&order, custody, mint, 122).is_err());
    }

    #[test]
    fn trigger_rules_match_take_profit_and_stop_loss_direction() {
        assert!(require_trigger_met(ORDER_KIND_TAKE_PROFIT, 101, 100).is_ok());
        assert!(require_trigger_met(ORDER_KIND_TAKE_PROFIT, 99, 100).is_err());
        assert!(require_trigger_met(ORDER_KIND_STOP_LOSS, 99, 100).is_ok());
        assert!(require_trigger_met(ORDER_KIND_STOP_LOSS, 101, 100).is_err());
        assert!(require_trigger_met(0, 100, 100).is_err());
    }

    #[test]
    fn approval_payload_binds_close_action_and_delegation() {
        let market = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let position = Pubkey::new_unique();
        let delegation = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let approval = LeverageDelegationApproval::new(
            LEVERAGE_DELEGATE_CLOSE,
            market,
            owner,
            position,
            delegation,
            dusk::state::MarketAsset::Base,
            recipient,
            mint,
            123,
        );
        let mut data = Vec::new();
        approval.serialize(&mut data).unwrap();
        let decoded = LeverageDelegationApproval::deserialize(&mut data.as_slice()).unwrap();

        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.action, LEVERAGE_DELEGATE_CLOSE);
        assert_eq!(decoded.market, market);
        assert_eq!(decoded.owner, owner);
        assert_eq!(decoded.position, position);
        assert_eq!(decoded.delegation, delegation);
        assert_eq!(decoded.debt_asset, dusk::state::MarketAsset::Base.code());
        assert_eq!(decoded.recipient_token_account, recipient);
        assert_eq!(decoded.output_mint, mint);
        assert_eq!(decoded.output_amount, 123);
    }
}
