use super::*;

pub(super) fn closeout_price_nad(
    closeout_value: u64,
    collateral_amount: u64,
    debt_decimals: u8,
    collateral_decimals: u8,
) -> Result<u64> {
    require!(collateral_amount > 0, LeverageDelegateError::InvalidOrder);
    let (numerator, denominator) = if collateral_decimals >= debt_decimals {
        let scale = 10_u128
            .checked_pow((collateral_decimals - debt_decimals) as u32)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        ((NAD as u128).checked_mul(scale), collateral_amount as u128)
    } else {
        let scale = 10_u128
            .checked_pow((debt_decimals - collateral_decimals) as u32)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        (
            Some(NAD as u128),
            (collateral_amount as u128)
                .checked_mul(scale)
                .ok_or(LeverageDelegateError::MathOverflow)?,
        )
    };
    (closeout_value as u128)
        .checked_mul(numerator.ok_or(LeverageDelegateError::MathOverflow)?)
        .and_then(|value| value.checked_div(denominator))
        .ok_or(LeverageDelegateError::MathOverflow)?
        .try_into()
        .map_err(|_| LeverageDelegateError::MathOverflow.into())
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
    /// Collateral mint is needed to reproduce the exact net reserve credit for
    /// Token-2022 transfer-fee assets before approving a partial close.
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
        let closeout_value = ctx.accounts.market.leverage_closeout_value_at_time(
            &ctx.accounts.leverage_position,
            current_slot,
            clock.unix_timestamp,
        )?;
        let closeout_price_nad = closeout_price_nad(
            closeout_value,
            ctx.accounts.leverage_position.collateral_amount,
            ctx.accounts.token_mint.decimals,
            ctx.accounts.collateral_mint.decimals,
        )?;
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
        let incentive_basis = if order.close_bps == BPS_DENOMINATOR {
            ctx.accounts.leverage_position.margin_amount
        } else {
            output_amount
        };
        let payout = quote_delegated_close_payout(
            &ctx.accounts.token_mint.to_account_info(),
            residual,
            close_quote.amount_out,
            incentive_basis,
            clock.epoch,
        )?;
        order.staged_margin = incentive_basis;
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
        order.staged_execution_value = close_quote.amount_out;
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
