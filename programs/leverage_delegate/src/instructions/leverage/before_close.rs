use super::*;

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
        constraint = market.assert_current_version().is_ok() @ LeverageDelegateError::InvalidMarketVersion
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
        constraint = owner_token_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        constraint = fee_recipient.owner == futarchy_authority.recipients.futarchy_treasury @ LeverageDelegateError::InvalidTokenAccount,
        constraint = fee_recipient.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub fee_recipient: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        constraint = executor_token_account.owner == executor.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = executor_token_account.mint == token_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub executor_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub futarchy_authority: Box<Account<'info, dusk::state::FutarchyAuthority>>,
    /// Used to verify the exact net reserve credit for Token-2022 collateral.
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Identifies the executor; Dusk does not forward signer privileges
    /// across the delegated callback boundary.
    pub executor: UncheckedAccount<'info>,
}

impl<'info> BeforeLeverageOrder<'info> {
    pub fn handle_before(
        ctx: Context<Self>,
        _args: ExecuteOrderArgs,
        expected_kind: u8,
    ) -> Result<()> {
        let order = &mut ctx.accounts.order;
        order.assert_position_generation(&ctx.accounts.leverage_position)?;
        require!(
            order.kind == expected_kind,
            LeverageDelegateError::InvalidOrder
        );
        let clock = Clock::get()?;
        let current_slot = clock.slot;
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
        // Dusk owns this read-only account and exposes this context only while
        // its close instruction is on the stack. Reuse its certified execution
        // quote instead of repeating the OP solver in a second program.
        let approval = ctx.accounts.leverage_position.close_approval;
        require!(
            approval.slot == current_slot
                && approval.close_bps == order.close_bps
                && approval.selected_output > 0
                && order.staged_collateral_amount == 0
                && order.min_owner_payout > 0,
            LeverageDelegateError::InvalidOrder
        );
        let risk = &ctx.accounts.market.risk;
        let ema_price_nad = match collateral_asset {
            MarketAsset::Base => risk.base_price_ema_nad,
            MarketAsset::Quote => risk.quote_price_ema_nad,
        };
        require!(
            risk.last_snapshot_slot == current_slot && ema_price_nad > 0,
            LeverageDelegateError::InvalidOrder
        );
        match expected_kind {
            ORDER_KIND_TAKE_PROFIT => require!(
                ema_price_nad >= order.trigger_price_nad,
                LeverageDelegateError::TriggerNotMet
            ),
            ORDER_KIND_STOP_LOSS => require!(
                ema_price_nad <= order.trigger_price_nad,
                LeverageDelegateError::TriggerNotMet
            ),
            _ => return err!(LeverageDelegateError::InvalidOrder),
        }
        // These three payout accounts must remain distinct.
        require_keys_neq!(
            ctx.accounts.owner_token_account.key(),
            ctx.accounts.fee_recipient.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_neq!(
            ctx.accounts.owner_token_account.key(),
            ctx.accounts.executor_token_account.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_neq!(
            ctx.accounts.fee_recipient.key(),
            ctx.accounts.executor_token_account.key(),
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
        require_eq!(
            collateral_credit,
            approval.collateral_credit,
            LeverageDelegateError::InvalidOrder
        );
        let selected_closeout = approval.selected_output;
        let cash_repaid = ctx
            .accounts
            .market
            .debt
            .isolated_repayment_for_max(debt_asset, close_slice.debt_shares, u64::MAX)?
            .cash_repaid;
        let residual = selected_closeout
            .checked_sub(cash_repaid)
            .ok_or(LeverageDelegateError::InvalidOrder)?;
        let output_amount = residual
            .checked_sub(get_transfer_fee(
                &ctx.accounts.token_mint.to_account_info(),
                residual,
            )?)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        let incentive_basis = if order.close_bps == BPS_DENOMINATOR {
            ctx.accounts.leverage_position.margin_amount
        } else {
            output_amount
        };
        let payout = quote_delegated_close_payout(
            &ctx.accounts.token_mint.to_account_info(),
            residual,
            selected_closeout,
            incentive_basis,
            clock.epoch,
        )?;
        require_gte!(
            payout.owner_credit,
            order.min_owner_payout,
            LeverageDelegateError::MinimumPayoutNotMet
        );
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
        order.staged_owner_token_account = ctx.accounts.owner_token_account.key();
        order.staged_owner_balance = ctx.accounts.owner_token_account.amount;
        order.staged_fee_recipient = ctx.accounts.fee_recipient.key();
        order.staged_fee_balance = ctx.accounts.fee_recipient.amount;
        order.staged_executor_token_account = ctx.accounts.executor_token_account.key();
        order.staged_executor_balance = ctx.accounts.executor_token_account.amount;
        order.staged_output_mint = ctx.accounts.token_mint.key();
        order.staged_output_amount = payout.owner_credit;
        order.staged_protocol_fee_debit = payout.protocol_debit;
        order.staged_protocol_fee_credit = payout.protocol_credit;
        order.staged_executor_credit = payout.executor_credit;
        order.staged_execution_value = selected_closeout;
        let approval = LeverageDelegationApproval::new(
            LEVERAGE_DELEGATE_CLOSE,
            ctx.accounts.market.key(),
            order.owner,
            ctx.accounts.leverage_position.key(),
            ctx.accounts.leverage_delegation.key(),
            debt_asset,
            ctx.accounts.owner_token_account.key(),
            ctx.accounts.token_mint.key(),
            close_slice.collateral_amount,
            payout.owner_credit,
        );
        let mut data = Vec::new();
        approval
            .serialize(&mut data)
            .map_err(|_| LeverageDelegateError::ApprovalSerializationFailed)?;
        set_return_data(&data);
        Ok(())
    }
}
