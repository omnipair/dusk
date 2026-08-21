use super::*;

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
