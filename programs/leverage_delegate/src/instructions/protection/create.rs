use super::*;

#[derive(Accounts)]
#[instruction(args: CreateProtectionOrderArgs)]
pub struct CreateProtectionOrder<'info> {
    pub market: Box<Account<'info, Market>>,
    pub borrow_position: Option<Box<Account<'info, BorrowPosition>>>,
    pub leverage_position: Option<Box<Account<'info, LeveragePosition>>>,
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(init, payer = owner, space = 8 + ProtectionOrder::INIT_SPACE,
        seeds = [PROTECTION_ORDER_SEED_PREFIX, owner.key().as_ref(), &args.order_id.to_le_bytes()], bump)]
    pub order: Box<Account<'info, ProtectionOrder>>,
    #[account(mut, constraint = owner_lp_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_lp_account.mint == lp_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub owner_lp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = custody_lp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_lp_account.mint == lp_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub custody_lp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    /// CHECK: Canonical Dusk event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}
impl<'info> CreateProtectionOrder<'info> {
    pub fn handle(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CreateProtectionOrderArgs,
    ) -> Result<()> {
        let a = ctx.accounts;
        let clock = Clock::get()?;
        a.market.assert_current_version()?;
        require!(
            a.custody_lp_account.delegate.is_none()
                && a.custody_lp_account.close_authority.is_none(),
            LeverageDelegateError::InvalidTokenAccount
        );
        let custody_ata =
            anchor_spl::associated_token::get_associated_token_address_with_program_id(
                &a.order.key(),
                &a.lp_mint.key(),
                &Token2022::id(),
            );
        require_keys_eq!(
            a.custody_lp_account.key(),
            custody_ata,
            LeverageDelegateError::InvalidTokenAccount
        );
        require!(
            args.action <= 2
                && args.lp_amount > 0
                && args.max_lp_per_execution > 0
                && args.max_payment_per_execution > 0
                && args.min_payment_per_lp_nad > 0
                && args.keeper_fee_bps <= 1_000
                && args.trigger_health_bps > BPS_DENOMINATOR as u64
                && args.target_health_bps > args.trigger_health_bps
                && args.expires_at > clock.unix_timestamp,
            LeverageDelegateError::InvalidOrder
        );
        let asset = MarketAsset::try_from_code(args.debt_asset)?;
        let payment_asset = if args.action == 1 {
            asset.opposite()
        } else {
            asset
        };
        let kind = if a.lp_mint.key() == a.market.ylp_mint {
            YieldTokenKind::Ylp
        } else {
            require!(
                a.market.asset_for_hlp_mint(a.lp_mint.key())? == payment_asset,
                LeverageDelegateError::InvalidOrder
            );
            YieldTokenKind::Hlp
        };
        let (position, position_owner) = if args.action < 2 {
            require!(
                a.leverage_position.is_none(),
                LeverageDelegateError::InvalidOrder
            );
            let p = a
                .borrow_position
                .as_ref()
                .ok_or(LeverageDelegateError::InvalidOrder)?;
            p.assert_position(p.owner, a.market.key())?;
            (p.key(), p.owner)
        } else {
            require!(
                a.borrow_position.is_none(),
                LeverageDelegateError::InvalidOrder
            );
            let p = a
                .leverage_position
                .as_ref()
                .ok_or(LeverageDelegateError::InvalidOrder)?;
            p.assert_position(p.owner, a.market.key(), asset)?;
            (p.key(), p.owner)
        };
        require_keys_eq!(
            a.base_mint.key(),
            a.market.base_side.asset_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            a.quote_mint.key(),
            a.market.quote_side.asset_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        a.order.set_inner(ProtectionOrder {
            owner: a.owner.key(),
            market: a.market.key(),
            position,
            position_owner,
            lp_mint: a.lp_mint.key(),
            custody_lp_account: a.custody_lp_account.key(),
            order_id: args.order_id,
            action: args.action,
            debt_asset: args.debt_asset,
            active: true,
            remaining_lp: args.lp_amount,
            max_lp_per_execution: args.max_lp_per_execution,
            max_payment_per_execution: args.max_payment_per_execution,
            trigger_health_bps: args.trigger_health_bps,
            target_health_bps: args.target_health_bps,
            min_payment_per_lp_nad: args.min_payment_per_lp_nad,
            keeper_fee_bps: args.keeper_fee_bps,
            expires_at: args.expires_at,
            bump: ctx.bumps.order,
        });
        let owner = a.owner.key();
        let id = args.order_id.to_le_bytes();
        let bump = [ctx.bumps.order];
        let seeds = &[PROTECTION_ORDER_SEED_PREFIX, owner.as_ref(), &id, &bump];
        for (mint, yield_account) in [
            (&a.base_mint, &a.base_yield_account),
            (&a.quote_mint, &a.quote_yield_account),
        ] {
            yield_account.assert_account(
                a.order.key(),
                a.market.key(),
                a.lp_mint.key(),
                mint.key(),
                kind,
            )?;
            dusk::cpi::set_yield_recipient(
                CpiContext::new_with_signer(
                    a.dusk_program.to_account_info(),
                    dusk::cpi::accounts::SetYieldRecipient {
                        market: a.market.to_account_info(),
                        owner: a.order.to_account_info(),
                        asset_mint: mint.to_account_info(),
                        lp_mint: a.lp_mint.to_account_info(),
                        yield_account: yield_account.to_account_info(),
                        event_authority: a.dusk_event_authority.to_account_info(),
                        program: a.dusk_program.to_account_info(),
                    },
                    &[seeds],
                ),
                dusk::instructions::SetYieldRecipientArgs {
                    token_kind: kind,
                    recipient: owner,
                },
            )?;
        }
        let mut hook_accounts = vec![
            a.market.to_account_info(),
            a.base_mint.to_account_info(),
            a.quote_mint.to_account_info(),
            a.base_yield_account.to_account_info(),
            a.quote_yield_account.to_account_info(),
            a.dusk_program.to_account_info(),
        ];
        hook_accounts.extend_from_slice(ctx.remaining_accounts);
        let before = a.custody_lp_account.amount;
        transfer_checked(
            a.token_2022_program.to_account_info(),
            a.owner_lp_account.to_account_info(),
            a.lp_mint.to_account_info(),
            a.custody_lp_account.to_account_info(),
            a.owner.to_account_info(),
            args.lp_amount,
            a.lp_mint.decimals,
            &[],
            &hook_accounts,
        )?;
        a.custody_lp_account.reload()?;
        require_eq!(
            a.custody_lp_account
                .amount
                .checked_sub(before)
                .ok_or(LeverageDelegateError::MathOverflow)?,
            args.lp_amount,
            LeverageDelegateError::InvalidTokenAccount
        );
        Ok(())
    }
}
