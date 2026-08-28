use super::*;

#[derive(Accounts)]
#[instruction(args: CreateHlpOrderArgs)]
pub struct CreateHlpOrder<'info> {
    #[account(
        constraint = market.version == MARKET_LAYOUT_VERSION @ LeverageDelegateError::InvalidMarketVersion
    )]
    pub market: Box<Account<'info, Market>>,
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = owner,
        space = 8 + HlpOrder::INIT_SPACE,
        seeds = [
            HLP_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            target_hlp_mint.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump
    )]
    pub order: Box<Account<'info, HlpOrder>>,
    #[account(
        mut,
        constraint = owner_hlp_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = custody_hlp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    /// CHECK: Canonical Dusk CPI-event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> CreateHlpOrder<'info> {
    pub fn handle_create(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CreateHlpOrderArgs,
    ) -> Result<()> {
        validate_hlp_order_kind(args.kind)?;
        require!(
            args.hlp_amount > 0 && args.trigger_nad > 0,
            LeverageDelegateError::InvalidOrder
        );
        ctx.accounts
            .market
            .asset_for_hlp_mint(ctx.accounts.target_hlp_mint.key())?;
        require_keys_eq!(
            ctx.accounts.base_mint.key(),
            ctx.accounts.market.base_side.asset_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.quote_mint.key(),
            ctx.accounts.market.quote_side.asset_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_gte!(
            ctx.accounts.owner_hlp_account.amount,
            args.hlp_amount,
            LeverageDelegateError::InvalidTokenAccount
        );
        let custody_balance_before = ctx.accounts.custody_hlp_account.amount;

        validate_hlp_yield_account(
            &ctx.accounts.base_yield_account,
            ctx.accounts.order.key(),
            ctx.accounts.market.key(),
            ctx.accounts.target_hlp_mint.key(),
            ctx.accounts.base_mint.key(),
        )?;
        validate_hlp_yield_account(
            &ctx.accounts.quote_yield_account,
            ctx.accounts.order.key(),
            ctx.accounts.market.key(),
            ctx.accounts.target_hlp_mint.key(),
            ctx.accounts.quote_mint.key(),
        )?;

        {
            let order = &mut ctx.accounts.order;
            order.owner = ctx.accounts.owner.key();
            order.market = ctx.accounts.market.key();
            order.target_hlp_mint = ctx.accounts.target_hlp_mint.key();
            order.custody_hlp_account = ctx.accounts.custody_hlp_account.key();
            order.order_id = args.order_id;
            order.kind = args.kind;
            order.status = HLP_ORDER_STATUS_ACTIVE;
            order.hlp_amount = args.hlp_amount;
            order.trigger_nad = args.trigger_nad;
            order.min_target_amount_out = args.min_target_amount_out;
            order.bump = ctx.bumps.order;
        }

        let market_key = ctx.accounts.order.market;
        let owner_key = ctx.accounts.order.owner;
        let target_hlp_mint_key = ctx.accounts.order.target_hlp_mint;
        let order_id_bytes = ctx.accounts.order.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let authority_seeds = &[
            HLP_ORDER_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            target_hlp_mint_key.as_ref(),
            &order_id_bytes,
            &bump_seed,
        ];
        let signer = &[&authority_seeds[..]];

        set_hlp_yield_recipient(
            ctx.accounts.dusk_program.to_account_info(),
            ctx.accounts.dusk_event_authority.to_account_info(),
            ctx.accounts.market.to_account_info(),
            ctx.accounts.order.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            ctx.accounts.base_yield_account.to_account_info(),
            owner_key,
            signer,
        )?;
        set_hlp_yield_recipient(
            ctx.accounts.dusk_program.to_account_info(),
            ctx.accounts.dusk_event_authority.to_account_info(),
            ctx.accounts.market.to_account_info(),
            ctx.accounts.order.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            ctx.accounts.quote_yield_account.to_account_info(),
            owner_key,
            signer,
        )?;

        let mut hook_accounts = vec![
            ctx.accounts.market.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            ctx.accounts.base_yield_account.to_account_info(),
            ctx.accounts.quote_yield_account.to_account_info(),
            ctx.accounts.dusk_program.to_account_info(),
        ];
        hook_accounts.extend_from_slice(ctx.remaining_accounts);
        transfer_checked(
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.accounts.owner_hlp_account.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.custody_hlp_account.to_account_info(),
            ctx.accounts.owner.to_account_info(),
            args.hlp_amount,
            ctx.accounts.target_hlp_mint.decimals,
            &[],
            &hook_accounts,
        )?;
        ctx.accounts.custody_hlp_account.reload()?;
        let custody_credit = ctx
            .accounts
            .custody_hlp_account
            .amount
            .checked_sub(custody_balance_before)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        require_eq!(
            custody_credit,
            args.hlp_amount,
            LeverageDelegateError::InvalidTokenAccount
        );
        Ok(())
    }
}
