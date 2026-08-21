use super::*;

#[derive(Accounts)]
#[instruction(args: HlpOrderIdArgs)]
pub struct SettleHlpOrderYield<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            HLP_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            order.owner.as_ref(),
            order.target_hlp_mint.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.market == market.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.status != HLP_ORDER_STATUS_ACTIVE @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, HlpOrder>>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        address = order.custody_hlp_account,
        constraint = custody_hlp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_hlp_account.amount == 0 @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    #[account(
        mut,
        constraint = owner_base_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_base_account.mint == base_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_base_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_quote_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_quote_account.mint == quote_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_quote_account: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Order owner receives account rent; yield recipients are checked by Dusk.
    #[account(mut, address = order.owner)]
    pub owner: AccountInfo<'info>,
    /// CHECK: Canonical Dusk CPI-event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> SettleHlpOrderYield<'info> {
    pub fn handle_settle(
        ctx: Context<'_, '_, '_, 'info, Self>,
        _args: HlpOrderIdArgs,
    ) -> Result<()> {
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
        require_keys_eq!(
            ctx.accounts.base_yield_account.recipient,
            ctx.accounts.order.owner,
            LeverageDelegateError::InvalidOrder
        );
        require_keys_eq!(
            ctx.accounts.quote_yield_account.recipient,
            ctx.accounts.order.owner,
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

        claim_hlp_yield_if_available(
            ctx.accounts.base_yield_account.accrued_swap_fee_amount,
            ctx.accounts.base_yield_account.accrued_interest_amount,
            ctx.accounts.dusk_program.to_account_info(),
            ctx.accounts.dusk_event_authority.to_account_info(),
            ctx.accounts.market.to_account_info(),
            ctx.accounts.order.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.custody_hlp_account.to_account_info(),
            ctx.accounts.base_reserve_vault.to_account_info(),
            ctx.accounts.base_interest_vault.to_account_info(),
            ctx.accounts.owner_base_account.to_account_info(),
            ctx.accounts.base_yield_account.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            signer,
            ctx.remaining_accounts,
        )?;
        ctx.accounts.quote_yield_account.reload()?;
        claim_hlp_yield_if_available(
            ctx.accounts.quote_yield_account.accrued_swap_fee_amount,
            ctx.accounts.quote_yield_account.accrued_interest_amount,
            ctx.accounts.dusk_program.to_account_info(),
            ctx.accounts.dusk_event_authority.to_account_info(),
            ctx.accounts.market.to_account_info(),
            ctx.accounts.order.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.custody_hlp_account.to_account_info(),
            ctx.accounts.quote_reserve_vault.to_account_info(),
            ctx.accounts.quote_interest_vault.to_account_info(),
            ctx.accounts.owner_quote_account.to_account_info(),
            ctx.accounts.quote_yield_account.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            signer,
            ctx.remaining_accounts,
        )?;
        Ok(())
    }
}
