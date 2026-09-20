use super::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct FundProtectionOrderArgs {
    pub lp_amount: u64,
}

#[derive(Accounts)]
pub struct ManageProtectionOrder<'info> {
    #[account(mut, seeds = [PROTECTION_ORDER_SEED_PREFIX, owner.key().as_ref(), &order.order_id.to_le_bytes()], bump = order.bump,
        constraint = order.owner == owner.key() @ LeverageDelegateError::InvalidOrder)]
    pub order: Box<Account<'info, ProtectionOrder>>,
    #[account(address = order.lp_mint)]
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, address = order.custody_lp_account,
        constraint = custody_lp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_lp_account.mint == lp_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub custody_lp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = owner_lp_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_lp_account.mint == lp_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub owner_lp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub owner: Signer<'info>,
    pub token_2022_program: Program<'info, Token2022>,
}
impl<'info> ManageProtectionOrder<'info> {
    pub fn fund(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: FundProtectionOrderArgs,
    ) -> Result<()> {
        let a = ctx.accounts;
        require!(
            a.order.active
                && args.lp_amount > 0
                && Clock::get()?.unix_timestamp < a.order.expires_at,
            LeverageDelegateError::InvalidOrder
        );
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
            ctx.remaining_accounts,
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
        a.order.remaining_lp = a
            .order
            .remaining_lp
            .checked_add(args.lp_amount)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        Ok(())
    }
    pub fn cancel(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let a = ctx.accounts;
        let owner = a.owner.key();
        let id = a.order.order_id.to_le_bytes();
        let bump = [a.order.bump];
        let seeds = &[PROTECTION_ORDER_SEED_PREFIX, owner.as_ref(), &id, &bump];
        transfer_checked(
            a.token_2022_program.to_account_info(),
            a.custody_lp_account.to_account_info(),
            a.lp_mint.to_account_info(),
            a.owner_lp_account.to_account_info(),
            a.order.to_account_info(),
            a.custody_lp_account.amount,
            a.lp_mint.decimals,
            &[seeds],
            ctx.remaining_accounts,
        )?;
        a.order.active = false;
        a.order.remaining_lp = 0;
        // Keep yield accounts and the order address alive. The sponsor remains
        // their designated recipient and may harvest earned yield after exit.
        Ok(())
    }
}
