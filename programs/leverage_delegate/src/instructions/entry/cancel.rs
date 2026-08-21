use super::*;

#[derive(Accounts)]
#[instruction(args: LeverageEntryOrderIdArgs)]
pub struct CancelLeverageEntryOrder<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            ENTRY_ORDER_SEED_PREFIX,
            order.market.as_ref(),
            owner.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.owner == owner.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, LeverageEntryOrder>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        address = order.funding_vault,
        constraint = funding_vault.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = funding_vault.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub funding_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_funding_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_funding_account.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_funding_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CancelLeverageEntryOrder<'info> {
    pub fn handle_cancel(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: LeverageEntryOrderIdArgs,
    ) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.debt_mint.key(),
            ctx.accounts.order.debt_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        let market_key = ctx.accounts.order.market;
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
        let amount = ctx.accounts.funding_vault.amount;
        if amount > 0 {
            transfer_checked(
                token_program_for_mint(
                    &ctx.accounts.debt_mint.to_account_info(),
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.funding_vault.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                ctx.accounts.owner_funding_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                amount,
                ctx.accounts.debt_mint.decimals,
                signer,
                ctx.remaining_accounts,
            )?;
        }
        ctx.accounts.funding_vault.reload()?;
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
