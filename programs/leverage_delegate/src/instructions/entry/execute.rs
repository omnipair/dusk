use super::*;

#[derive(Accounts)]
#[instruction(args: LeverageEntryOrderIdArgs)]
pub struct ExecuteLeverageEntryOrder<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            ENTRY_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            order.owner.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.market == market.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, LeverageEntryOrder>>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    /// CHECK: Receives order/vault rent and is bound by the stored order owner.
    #[account(mut, address = order.owner)]
    pub owner: AccountInfo<'info>,
    /// CHECK: Dusk initializes and validates the canonical position PDA.
    #[account(mut, address = order.position)]
    pub leverage_position: UncheckedAccount<'info>,
    #[account(address = order.debt_mint)]
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address = order.collateral_mint)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Dusk validates/initializes the canonical shared collateral vault.
    #[account(mut)]
    pub leverage_collateral_vault: UncheckedAccount<'info>,
    #[account(
        mut,
        constraint = funding_vault.key() == leverage_entry_funding_vault_address(
            order.key(),
            debt_mint.key(),
            *debt_mint.to_account_info().owner,
        ) @ LeverageDelegateError::InvalidTokenAccount,
        constraint = funding_vault.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = funding_vault.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub funding_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_refund_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_refund_account.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_refund_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = executor_bounty_account.owner == executor.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = executor_bounty_account.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub executor_bounty_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,
    /// CHECK: Canonical Instructions sysvar, revalidated by Dusk.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    #[account(mut)]
    pub executor: Signer<'info>,
    /// CHECK: Canonical Dusk CPI-event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> ExecuteLeverageEntryOrder<'info> {
    pub fn handle_execute(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: LeverageEntryOrderIdArgs,
    ) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(
            now <= ctx.accounts.order.expiry_unix_timestamp,
            LeverageDelegateError::InvalidOrder
        );
        require!(
            ctx.accounts.market.version == MARKET_LAYOUT_VERSION,
            LeverageDelegateError::InvalidMarketVersion
        );
        let expected_escrow = ctx
            .accounts
            .order
            .margin_amount
            .checked_add(ctx.accounts.order.executor_bounty)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        require_gte!(
            ctx.accounts.funding_vault.amount,
            expected_escrow,
            LeverageDelegateError::InvalidTokenAccount
        );
        let debt_asset = MarketAsset::try_from_code(ctx.accounts.order.debt_asset)?;
        let hook_account_offset = if ctx.accounts.market.has_active_hlp() {
            LEVERAGE_HLP_ACCOUNT_PREFIX_LEN
        } else {
            0
        };
        require_keys_eq!(
            ctx.accounts.market.side(debt_asset).asset_mint,
            ctx.accounts.debt_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.market.side(debt_asset.opposite()).asset_mint,
            ctx.accounts.collateral_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );

        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.order.owner;
        let order_id_bytes = args.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let authority_seeds = &[
            ENTRY_ORDER_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            &order_id_bytes,
            &bump_seed,
        ];
        let signer = &[&authority_seeds[..]];

        dusk::cpi::open_leverage(
            CpiContext::new_with_signer(
                ctx.accounts.dusk_program.to_account_info(),
                dusk::cpi::accounts::OpenLeverage {
                    market: ctx.accounts.market.to_account_info(),
                    futarchy_authority: ctx.accounts.futarchy_authority.to_account_info(),
                    owner: ctx.accounts.order.to_account_info(),
                    payer: ctx.accounts.executor.to_account_info(),
                    leverage_position: ctx.accounts.leverage_position.to_account_info(),
                    debt_mint: ctx.accounts.debt_mint.to_account_info(),
                    collateral_mint: ctx.accounts.collateral_mint.to_account_info(),
                    debt_reserve_vault: ctx.accounts.debt_reserve_vault.to_account_info(),
                    collateral_reserve_vault: ctx
                        .accounts
                        .collateral_reserve_vault
                        .to_account_info(),
                    leverage_collateral_vault: ctx
                        .accounts
                        .leverage_collateral_vault
                        .to_account_info(),
                    owner_debt_account: ctx.accounts.funding_vault.to_account_info(),
                    referral_partner: ctx
                        .accounts
                        .referral_partner
                        .as_ref()
                        .map(|account| account.to_account_info()),
                    referral_accrual: ctx
                        .accounts
                        .referral_accrual
                        .as_ref()
                        .map(|account| account.to_account_info()),
                    instructions_sysvar: ctx.accounts.instructions_sysvar.to_account_info(),
                    token_program: ctx.accounts.token_program.to_account_info(),
                    token_2022_program: ctx.accounts.token_2022_program.to_account_info(),
                    system_program: ctx.accounts.system_program.to_account_info(),
                    event_authority: ctx.accounts.dusk_event_authority.to_account_info(),
                    program: ctx.accounts.dusk_program.to_account_info(),
                },
                signer,
            )
            .with_remaining_accounts(ctx.remaining_accounts.to_vec()),
            OpenLeverageArgs {
                position_id: ctx.accounts.order.position_id,
                debt_asset: ctx.accounts.order.debt_asset,
                margin_amount: ctx.accounts.order.margin_amount,
                multiplier_bps: ctx.accounts.order.multiplier_bps,
                min_collateral_out: ctx.accounts.order.min_collateral_out,
                referrer: ctx.accounts.order.referrer,
                position_owner: Some(ctx.accounts.order.owner),
                limit_price_nad: ctx.accounts.order.limit_price_nad,
            },
        )?;

        ctx.accounts.funding_vault.reload()?;
        require_gte!(
            ctx.accounts.funding_vault.amount,
            ctx.accounts.order.executor_bounty,
            LeverageDelegateError::InvalidTokenAccount
        );
        verify_opened_position(&ctx.accounts.leverage_position, &ctx.accounts.order)?;

        let hook_accounts = ctx
            .remaining_accounts
            .get(hook_account_offset..)
            .ok_or(LeverageDelegateError::InvalidOrder)?;
        if ctx.accounts.order.executor_bounty > 0 {
            transfer_checked(
                token_program_for_mint(
                    &ctx.accounts.debt_mint.to_account_info(),
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.funding_vault.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                ctx.accounts.executor_bounty_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                ctx.accounts.order.executor_bounty,
                ctx.accounts.debt_mint.decimals,
                signer,
                hook_accounts,
            )?;
        }
        ctx.accounts.funding_vault.reload()?;
        let surplus = ctx.accounts.funding_vault.amount;
        if surplus > 0 {
            transfer_checked(
                token_program_for_mint(
                    &ctx.accounts.debt_mint.to_account_info(),
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.funding_vault.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                ctx.accounts.owner_refund_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                surplus,
                ctx.accounts.debt_mint.decimals,
                signer,
                hook_accounts,
            )?;
            ctx.accounts.funding_vault.reload()?;
        }
        require_eq!(
            ctx.accounts.funding_vault.amount,
            0,
            LeverageDelegateError::InvalidTokenAccount
        );
        close_token_account(
            token_program_for_mint(
                &ctx.accounts.debt_mint.to_account_info(),
                &ctx.accounts.token_program.to_account_info(),
                &ctx.accounts.token_2022_program.to_account_info(),
            ),
            ctx.accounts.funding_vault.to_account_info(),
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.order.to_account_info(),
            signer,
        )
    }
}
