use super::*;

#[derive(Accounts)]
#[instruction(args: HlpOrderIdArgs)]
pub struct CancelHlpOrder<'info> {
    #[account(
        mut,
        seeds = [
            HLP_ORDER_SEED_PREFIX,
            order.market.as_ref(),
            owner.key().as_ref(),
            order.target_hlp_mint.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.owner == owner.key() @ LeverageDelegateError::InvalidOrder,
        constraint = order.status == HLP_ORDER_STATUS_ACTIVE @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, HlpOrder>>,
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        address = order.custody_hlp_account,
        constraint = custody_hlp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub custody_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_hlp_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_hlp_account.mint == target_hlp_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub owner: Signer<'info>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CancelHlpOrder<'info> {
    pub fn handle_cancel(
        ctx: Context<'_, '_, '_, 'info, Self>,
        _args: HlpOrderIdArgs,
    ) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.target_hlp_mint.key(),
            ctx.accounts.order.target_hlp_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_eq!(
            ctx.accounts.custody_hlp_account.amount,
            ctx.accounts.order.hlp_amount,
            LeverageDelegateError::InvalidTokenAccount
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
        token_2022::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_2022_program.to_account_info(),
                token_2022::TransferChecked {
                    from: ctx.accounts.custody_hlp_account.to_account_info(),
                    mint: ctx.accounts.target_hlp_mint.to_account_info(),
                    to: ctx.accounts.owner_hlp_account.to_account_info(),
                    authority: ctx.accounts.order.to_account_info(),
                },
                &[&authority_seeds[..]],
            )
            .with_remaining_accounts(ctx.remaining_accounts.to_vec()),
            ctx.accounts.order.hlp_amount,
            ctx.accounts.target_hlp_mint.decimals,
        )?;
        ctx.accounts.custody_hlp_account.reload()?;
        require_eq!(
            ctx.accounts.custody_hlp_account.amount,
            0,
            LeverageDelegateError::InvalidTokenAccount
        );
        ctx.accounts.order.status = HLP_ORDER_STATUS_CANCELLED;
        Ok(())
    }
}
