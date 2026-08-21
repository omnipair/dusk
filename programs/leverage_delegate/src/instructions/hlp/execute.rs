use super::*;

#[derive(Accounts)]
#[instruction(args: HlpOrderIdArgs)]
pub struct ExecuteHlpOrder<'info> {
    #[account(
        mut,
        seeds = [
            HLP_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            order.owner.as_ref(),
            order.target_hlp_mint.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.market == market.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.status == HLP_ORDER_STATUS_ACTIVE @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, HlpOrder>>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, address = order.target_hlp_mint)]
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub borrowed_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = custody_target_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        address = order.custody_hlp_account,
        constraint = custody_hlp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub hlp_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(
        mut,
        constraint = owner_target_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = executor_target_account.owner == executor.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub executor_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub executor: Signer<'info>,
    /// CHECK: Canonical Dusk CPI-event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> ExecuteHlpOrder<'info> {
    pub fn handle_execute(
        ctx: Context<'_, '_, '_, 'info, Self>,
        _args: HlpOrderIdArgs,
    ) -> Result<()> {
        require!(
            ctx.accounts.market.version == MARKET_LAYOUT_VERSION,
            LeverageDelegateError::InvalidMarketVersion
        );
        require_eq!(
            ctx.accounts.custody_hlp_account.amount,
            ctx.accounts.order.hlp_amount,
            LeverageDelegateError::InvalidTokenAccount
        );
        require!(
            ctx.accounts.custody_target_account.amount == 0,
            LeverageDelegateError::InvalidTokenAccount
        );

        let target_asset = ctx
            .accounts
            .market
            .asset_for_hlp_mint(ctx.accounts.target_hlp_mint.key())?;
        let trigger_preview =
            preview_hlp_order_trigger(ctx.accounts, target_asset, ctx.accounts.order.hlp_amount)?;

        let target_mint = match target_asset {
            MarketAsset::Base => ctx.accounts.base_mint.key(),
            MarketAsset::Quote => ctx.accounts.quote_mint.key(),
        };
        require_keys_eq!(
            ctx.accounts.custody_target_account.mint,
            target_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.owner_target_account.mint,
            target_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.executor_target_account.mint,
            target_mint,
            LeverageDelegateError::InvalidTokenAccount
        );

        let (principal_nav, funding_apr) = match ctx.accounts.order.kind {
            HLP_ORDER_KIND_STOP_LOSS => (trigger_preview.principal_nav_per_token_nad, 0),
            HLP_ORDER_KIND_STOP_RATE => (0, trigger_preview.funding_apr_ema_nad),
            _ => return err!(LeverageDelegateError::InvalidOrder),
        };
        require!(
            hlp_order_trigger_met(
                ctx.accounts.order.kind,
                principal_nav,
                funding_apr,
                ctx.accounts.order.trigger_nad,
            )?,
            LeverageDelegateError::TriggerNotMet
        );

        withdraw_hlp_order_position(ctx.accounts, ctx.remaining_accounts)?;
        ctx.accounts.custody_target_account.reload()?;
        let output = ctx.accounts.custody_target_account.amount;
        require_gte!(
            output,
            ctx.accounts.order.min_target_amount_out,
            LeverageDelegateError::InvalidOrder
        );
        let incentive = min(
            output,
            ceil_div(
                (output as u128)
                    .checked_mul(EXECUTOR_INCENTIVE_BPS as u128)
                    .ok_or(LeverageDelegateError::MathOverflow)?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(LeverageDelegateError::MathOverflow)? as u64,
        );
        let owner_amount = output
            .checked_sub(incentive)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        require_gte!(
            owner_amount,
            ctx.accounts.order.min_target_amount_out,
            LeverageDelegateError::InvalidOrder
        );
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
        let target_mint_account = match target_asset {
            MarketAsset::Base => ctx.accounts.base_mint.to_account_info(),
            MarketAsset::Quote => ctx.accounts.quote_mint.to_account_info(),
        };
        let target_decimals = match target_asset {
            MarketAsset::Base => ctx.accounts.base_mint.decimals,
            MarketAsset::Quote => ctx.accounts.quote_mint.decimals,
        };
        if incentive > 0 {
            transfer_checked_with_signer(
                token_program_for_mint(
                    &target_mint_account,
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.custody_target_account.to_account_info(),
                target_mint_account.clone(),
                ctx.accounts.executor_target_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                incentive,
                target_decimals,
                signer,
            )?;
        }
        if owner_amount > 0 {
            transfer_checked_with_signer(
                token_program_for_mint(
                    &target_mint_account,
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.custody_target_account.to_account_info(),
                target_mint_account,
                ctx.accounts.owner_target_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                owner_amount,
                target_decimals,
                signer,
            )?;
        }
        ctx.accounts.order.status = HLP_ORDER_STATUS_EXECUTED;
        Ok(())
    }
}
