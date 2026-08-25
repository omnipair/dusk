use super::*;

#[derive(Accounts)]
#[instruction(args: CreateLeverageEntryOrderArgs)]
pub struct CreateLeverageEntryOrder<'info> {
    #[account(
        constraint = market.version == MARKET_LAYOUT_VERSION @ LeverageDelegateError::InvalidMarketVersion
    )]
    pub market: Box<Account<'info, Market>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = owner,
        space = 8 + LeverageEntryOrder::INIT_SPACE,
        seeds = [
            ENTRY_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump
    )]
    pub order: Box<Account<'info, LeverageEntryOrder>>,
    #[account(
        mut,
        constraint = owner_funding_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_funding_account.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_funding_account: Box<InterfaceAccount<'info, TokenAccount>>,
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
    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> CreateLeverageEntryOrder<'info> {
    pub fn handle_create(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CreateLeverageEntryOrderArgs,
    ) -> Result<()> {
        require!(
            args.deposit_amount > 0
                && args.min_margin_amount > 0
                && args.limit_price_nad > 0
                && args.multiplier_bps > BPS_DENOMINATOR as u64
                && args.multiplier_bps <= LEVERAGE_MAX_MULTIPLIER_BPS,
            LeverageDelegateError::InvalidOrder
        );
        let now = Clock::get()?.unix_timestamp;
        require!(
            args.expiry_unix_timestamp > now,
            LeverageDelegateError::InvalidOrder
        );
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
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
        let (position, _) = leverage_position_pda(ctx.accounts.market.key(), args.position_id)?;

        let vault_balance_before = ctx.accounts.funding_vault.amount;
        transfer_checked(
            token_program_for_mint(
                &ctx.accounts.debt_mint.to_account_info(),
                &ctx.accounts.token_program.to_account_info(),
                &ctx.accounts.token_2022_program.to_account_info(),
            ),
            ctx.accounts.owner_funding_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            ctx.accounts.funding_vault.to_account_info(),
            ctx.accounts.owner.to_account_info(),
            args.deposit_amount,
            ctx.accounts.debt_mint.decimals,
            &[],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.funding_vault.reload()?;
        let credited = ctx
            .accounts
            .funding_vault
            .amount
            .checked_sub(vault_balance_before)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        let margin_amount =
            escrow_margin_after_bounty(credited, args.executor_bounty, args.min_margin_amount)?;

        let order = &mut ctx.accounts.order;
        order.owner = ctx.accounts.owner.key();
        order.market = ctx.accounts.market.key();
        order.position = position;
        order.position_id = args.position_id;
        order.debt_mint = ctx.accounts.debt_mint.key();
        order.collateral_mint = ctx.accounts.collateral_mint.key();
        order.order_id = args.order_id;
        order.debt_asset = args.debt_asset;
        order.margin_amount = margin_amount;
        order.executor_bounty = args.executor_bounty;
        order.multiplier_bps = args.multiplier_bps;
        order.limit_price_nad = args.limit_price_nad;
        order.min_collateral_out = args.min_collateral_out;
        order.expiry_unix_timestamp = args.expiry_unix_timestamp;
        order.referrer = args.referrer;
        order.bump = ctx.bumps.order;
        Ok(())
    }
}
