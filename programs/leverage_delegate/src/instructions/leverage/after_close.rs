use super::*;
use crate::instructions::fees::{order_protocol_fee, OrderProtocolFeePaid};

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
    )]
    pub leverage_delegation: Box<Account<'info, LeverageDelegation>>,
    #[account(
        constraint = owner_token_account.key() == order.staged_owner_token_account @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_token_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        constraint = executor_token_account.key() == order.staged_executor_token_account @ LeverageDelegateError::InvalidTokenAccount,
        constraint = executor_token_account.owner == executor.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = executor_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub executor_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        constraint = fee_recipient.key() == order.staged_fee_recipient @ LeverageDelegateError::InvalidTokenAccount,
        constraint = fee_recipient.owner == futarchy_authority.recipients.futarchy_treasury @ LeverageDelegateError::InvalidTokenAccount,
        constraint = fee_recipient.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub fee_recipient: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(constraint = token_mint.key() == order.staged_output_mint @ LeverageDelegateError::InvalidTokenAccount)]
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Identifies the executor; Dusk does not forward signer privileges
    /// across the delegated callback boundary.
    pub executor: UncheckedAccount<'info>,
    pub futarchy_authority: Box<Account<'info, dusk::state::FutarchyAuthority>>,
}

impl<'info> AfterCloseOrder<'info> {
    pub fn handle_after(
        ctx: Context<'_, '_, '_, 'info, Self>,
        _args: ExecuteOrderArgs,
    ) -> Result<()> {
        let order = &ctx.accounts.order;
        order.assert_position_generation(&ctx.accounts.leverage_position)?;
        require_eq!(
            ctx.accounts.leverage_position.debt_shares,
            order.staged_remaining_debt_shares,
            LeverageDelegateError::InvalidOrder
        );
        require_eq!(
            ctx.accounts.leverage_position.debt_principal,
            order.staged_remaining_debt_principal,
            LeverageDelegateError::InvalidOrder
        );
        require_eq!(
            ctx.accounts.leverage_position.collateral_amount,
            order.staged_remaining_collateral_amount,
            LeverageDelegateError::InvalidOrder
        );
        require_eq!(
            ctx.accounts.owner_token_account.amount,
            order
                .staged_owner_balance
                .checked_add(order.staged_output_amount)
                .ok_or(LeverageDelegateError::MathOverflow)?,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_eq!(
            ctx.accounts.fee_recipient.amount,
            order
                .staged_fee_balance
                .checked_add(order.staged_protocol_fee_credit)
                .ok_or(LeverageDelegateError::MathOverflow)?,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_eq!(
            ctx.accounts.executor_token_account.amount,
            order
                .staged_executor_balance
                .checked_add(order.staged_executor_credit)
                .ok_or(LeverageDelegateError::MathOverflow)?,
            LeverageDelegateError::InvalidTokenAccount
        );

        emit!(OrderProtocolFeePaid {
            order: order.key(),
            owner: order.owner,
            mint: ctx.accounts.token_mint.key(),
            value: order.staged_execution_value,
            fee: order_protocol_fee(order.staged_execution_value),
            debit: order.staged_protocol_fee_debit,
            credited: order.staged_protocol_fee_credit,
        });

        let approval = LeverageDelegationApproval::new(
            LEVERAGE_DELEGATE_CLOSE_SETTLED,
            order.market,
            order.owner,
            order.position,
            ctx.accounts.leverage_delegation.key(),
            ctx.accounts.leverage_delegation.debt_asset()?,
            ctx.accounts.owner_token_account.key(),
            ctx.accounts.token_mint.key(),
            order.staged_collateral_amount,
            order.staged_output_amount,
        );
        let mut data = Vec::new();
        approval
            .serialize(&mut data)
            .map_err(|_| LeverageDelegateError::ApprovalSerializationFailed)?;
        set_return_data(&data);
        Ok(())
    }
}
