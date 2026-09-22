use super::*;
use crate::instructions::fees::*;

#[derive(Accounts)]
pub struct ExecuteProtectionOrder<'info> {
    #[account(mut, seeds = [PROTECTION_ORDER_SEED_PREFIX, order.owner.as_ref(), &order.order_id.to_le_bytes()], bump = order.bump,
        constraint = order.active @ LeverageDelegateError::InvalidOrder)]
    pub order: Box<Account<'info, ProtectionOrder>>,
    #[account(mut, address = order.market)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    #[account(mut)]
    pub borrow_position: Option<Box<Account<'info, BorrowPosition>>>,
    #[account(mut)]
    pub leverage_position: Option<Box<Account<'info, LeveragePosition>>>,
    /// CHECK: Bound to the target and order; never receives funds.
    #[account(address = order.position_owner)]
    pub position_owner: AccountInfo<'info>,
    #[account(address = market.base_side.asset_mint)]
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address = market.quote_side.asset_mint)]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, address = order.lp_mint)]
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Native repayment reserve or collateral donation vault, validated by Dusk.
    #[account(mut)]
    pub payment_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub debt_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub borrowed_interest_vault: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    #[account(mut)]
    pub hlp_ylp_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    #[account(mut, address = order.custody_lp_account,
        constraint = custody_lp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_lp_account.mint == lp_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub custody_lp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = custody_base_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_base_account.mint == base_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub custody_base_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = custody_quote_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_quote_account.mint == quote_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub custody_quote_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut, constraint = owner_base_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_base_account.mint == base_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub owner_base_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = owner_quote_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_quote_account.mint == quote_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub owner_quote_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = keeper_payment_account.owner == keeper.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub keeper_payment_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,
    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,
    #[account(mut)]
    pub keeper: Signer<'info>,
    /// CHECK: Canonical Dusk CPI event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub protocol_fee: OrderFeePayment<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> ExecuteProtectionOrder<'info> {
    pub fn handle(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: ExecuteProtectionOrderArgs,
    ) -> Result<()> {
        let a = ctx.accounts;
        let clock = Clock::get()?;
        for custody in [&a.custody_base_account, &a.custody_quote_account] {
            require!(
                custody.delegate.is_none() && custody.close_authority.is_none(),
                LeverageDelegateError::InvalidTokenAccount
            );
        }
        require!(
            clock.unix_timestamp < a.order.expires_at
                && args.lp_amount > 0
                && args.payment_amount > 0,
            LeverageDelegateError::InvalidOrder
        );
        require!(
            args.lp_amount <= a.order.remaining_lp
                && args.lp_amount <= a.order.max_lp_per_execution
                && args.payment_amount <= a.order.max_payment_per_execution,
            LeverageDelegateError::InvalidOrder
        );
        let asset = MarketAsset::try_from_code(a.order.debt_asset)?;
        let payment_asset = if a.order.action == 1 {
            asset.opposite()
        } else {
            asset
        };
        let payment_mint = if payment_asset == MarketAsset::Base {
            &a.base_mint
        } else {
            &a.quote_mint
        };
        require_keys_eq!(
            a.keeper_payment_account.mint,
            payment_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        let payment_mint_info = payment_mint.to_account_info();
        let payment_decimals = payment_mint.decimals;
        if a.order.action < 2 {
            let p = a
                .borrow_position
                .as_ref()
                .ok_or(LeverageDelegateError::InvalidOrder)?;
            require_keys_eq!(
                p.key(),
                a.order.position,
                LeverageDelegateError::InvalidOrder
            );
            p.assert_position(a.order.position_owner, a.market.key())?;
        } else {
            let p = a
                .leverage_position
                .as_ref()
                .ok_or(LeverageDelegateError::InvalidOrder)?;
            require_keys_eq!(
                p.key(),
                a.order.position,
                LeverageDelegateError::InvalidOrder
            );
            p.assert_position(a.order.position_owner, a.market.key(), asset)?;
        }
        let before = protection_health_of(
            &mut a.market,
            a.borrow_position.as_deref().map(|p| &**p),
            a.leverage_position.as_deref().map(|p| &**p),
            a.order.action,
            asset,
            &clock,
            true,
        )?;
        require!(
            before <= a.order.trigger_health_bps,
            LeverageDelegateError::TriggerNotMet
        );
        let keeper_before = a.keeper_payment_account.amount;
        a.make_payment(
            args.payment_amount,
            payment_mint_info.clone(),
            ctx.remaining_accounts,
        )?;
        a.keeper_payment_account.reload()?;
        let paid = keeper_before
            .checked_sub(a.keeper_payment_account.amount)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        require!(
            paid > 0
                && (paid as u128) * 1_000_000_000
                    >= (args.lp_amount as u128) * a.order.min_payment_per_lp_nad as u128,
            LeverageDelegateError::InvalidOrder
        );
        let reward =
            ((paid as u128) * a.order.keeper_fee_bps as u128 / BPS_DENOMINATOR as u128) as u64;
        let reimbursement = paid
            .checked_add(reward)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        let gross = reimbursement
            .checked_add(dusk::token::get_transfer_inverse_fee_for_epoch(
                &payment_mint_info,
                reimbursement,
                clock.epoch,
            )?)
            .ok_or(LeverageDelegateError::MathOverflow)?;

        let payment_credit = paid
            .checked_sub(dusk::token::get_transfer_fee_for_epoch(
                &payment_mint_info,
                paid,
                clock.epoch,
            )?)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        let protocol_fee = crate::instructions::fees::order_protocol_fee(payment_credit);
        let protocol_debit = protocol_fee
            .checked_add(dusk::token::get_transfer_inverse_fee_for_epoch(
                &payment_mint_info,
                protocol_fee,
                clock.epoch,
            )?)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        let required_output = gross
            .checked_add(protocol_debit)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        let owner = a.order.owner;
        let id = a.order.order_id.to_le_bytes();
        let bump = [a.order.bump];
        let seeds = &[PROTECTION_ORDER_SEED_PREFIX, owner.as_ref(), &id, &bump];
        let base_before = a.custody_base_account.amount;
        let quote_before = a.custody_quote_account.amount;
        a.redeem_lp(
            args.lp_amount,
            payment_asset,
            required_output,
            &[seeds],
            ctx.remaining_accounts,
        )?;
        a.custody_base_account.reload()?;
        a.custody_quote_account.reload()?;
        let (custody, output, baseline) = if payment_asset == MarketAsset::Base {
            (
                a.custody_base_account.to_account_info(),
                a.custody_base_account.amount,
                base_before,
            )
        } else {
            (
                a.custody_quote_account.to_account_info(),
                a.custody_quote_account.amount,
                quote_before,
            )
        };
        require_gte!(
            output
                .checked_sub(baseline)
                .ok_or(LeverageDelegateError::MathOverflow)?,
            required_output,
            LeverageDelegateError::InvalidOrder
        );
        let keeper_after_payment = a.keeper_payment_account.amount;
        transfer_checked(
            token_program_for_mint(
                &payment_mint_info,
                &a.token_program.to_account_info(),
                &a.token_2022_program.to_account_info(),
            ),
            custody.clone(),
            payment_mint_info,
            a.keeper_payment_account.to_account_info(),
            a.order.to_account_info(),
            gross,
            payment_decimals,
            &[seeds],
            ctx.remaining_accounts,
        )?;
        a.keeper_payment_account.reload()?;
        require_gte!(
            a.keeper_payment_account
                .amount
                .checked_sub(keeper_after_payment)
                .ok_or(LeverageDelegateError::MathOverflow)?,
            reimbursement,
            LeverageDelegateError::InvalidOrder
        );

        a.protocol_fee.collect(
            owner,
            a.order.key(),
            if payment_asset == MarketAsset::Base {
                &a.base_mint
            } else {
                &a.quote_mint
            },
            &a.futarchy_authority,
            payment_credit,
            custody,
            a.order.to_account_info(),
            &[seeds],
            &a.token_program.to_account_info(),
            &a.token_2022_program.to_account_info(),
            ctx.remaining_accounts,
        )?;
        a.custody_base_account.reload()?;
        a.custody_quote_account.reload()?;
        // All unused proceeds, including the other yLP asset, belong to the sponsor.
        for (mint, custody, recipient) in [
            (&a.base_mint, &a.custody_base_account, &a.owner_base_account),
            (
                &a.quote_mint,
                &a.custody_quote_account,
                &a.owner_quote_account,
            ),
        ] {
            transfer_checked(
                token_program_for_mint(
                    &mint.to_account_info(),
                    &a.token_program.to_account_info(),
                    &a.token_2022_program.to_account_info(),
                ),
                custody.to_account_info(),
                mint.to_account_info(),
                recipient.to_account_info(),
                a.order.to_account_info(),
                custody.amount,
                mint.decimals,
                &[seeds],
                ctx.remaining_accounts,
            )?;
        }
        if let Some(p) = &mut a.borrow_position {
            p.reload()?;
        }
        if let Some(p) = &mut a.leverage_position {
            p.reload()?;
        }
        // A position whose debt shares reached zero is debt-free at every
        // borrow index, so its health is u64::MAX without decoding the large
        // Market a second time. Dusk's accounting events pushed the full
        // repayment path past the transaction compute budget with that decode.
        let debt_cleared = match (
            a.order.action,
            a.borrow_position.as_deref(),
            a.leverage_position.as_deref(),
        ) {
            (0 | 1, Some(position), None) => match asset {
                MarketAsset::Base => position.fixed_base_shares == 0,
                MarketAsset::Quote => position.fixed_quote_shares == 0,
            },
            (2, None, Some(position)) => position.debt_shares == 0,
            _ => return err!(LeverageDelegateError::InvalidOrder),
        };
        let after = if debt_cleared {
            u64::MAX
        } else {
            protection_health(
                &a.market.to_account_info(),
                a.borrow_position.as_deref().map(|p| &**p),
                a.leverage_position.as_deref().map(|p| &**p),
                a.order.action,
                asset,
                &clock,
                // Both native LP exits finalize risk after their reserve mutation.
                false,
            )?
        };
        require!(
            after >= a.order.target_health_bps && after > before,
            LeverageDelegateError::InvalidOrder
        );
        a.order.remaining_lp = a
            .order
            .remaining_lp
            .checked_sub(args.lp_amount)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        emit!(ProtectionExecuted {
            order: a.order.key(),
            position: a.order.position,
            keeper: a.keeper.key(),
            lp_burned: args.lp_amount,
            payment: paid,
            reward,
            health_before_bps: before,
            health_after_bps: after
        });
        Ok(())
    }
}
