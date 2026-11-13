use super::*;

/// Close a cancelled order after its LP escrow is spent or refunded and its
/// earned yield is harvested. The order and its custody token accounts return
/// their rent to the sponsor; any asset balance left in custody goes to the
/// sponsor's token account first. Dusk keeps the order's yield accounts.
#[derive(Accounts)]
pub struct CloseProtectionOrder<'info> {
    #[account(mut, close = owner,
        seeds = [PROTECTION_ORDER_SEED_PREFIX, owner.key().as_ref(), &order.order_id.to_le_bytes()], bump = order.bump,
        constraint = order.owner == owner.key() @ LeverageDelegateError::InvalidOrder,
        constraint = !order.active && order.remaining_lp == 0 @ LeverageDelegateError::InvalidOrder)]
    pub order: Box<Account<'info, ProtectionOrder>>,
    /// Must be empty. Cancellation refunds the whole balance, so repeating it
    /// returns a later LP transfer into custody.
    #[account(mut, address = order.custody_lp_account,
        constraint = custody_lp_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_lp_account.amount == 0 @ LeverageDelegateError::InvalidTokenAccount)]
    pub custody_lp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub base_yield_account: Box<Account<'info, YieldAccount>>,
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
    /// Writable so Token-2022 can harvest withheld transfer fees before close.
    #[account(mut)]
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    /// Order-owned asset custody that execution used, usually the order's ATA.
    #[account(mut, constraint = custody_base_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_base_account.mint == base_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub custody_base_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    #[account(mut, constraint = custody_quote_account.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = custody_quote_account.mint == quote_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub custody_quote_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    /// Required only when the matching custody account holds a balance.
    #[account(mut, constraint = owner_base_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_base_account.mint == base_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub owner_base_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    #[account(mut, constraint = owner_quote_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_quote_account.mint == quote_mint.key() @ LeverageDelegateError::InvalidTokenAccount)]
    pub owner_quote_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CloseProtectionOrder<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let a = ctx.accounts;
        let order_key = a.order.key();
        require_keys_neq!(
            a.base_mint.key(),
            a.quote_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        // Dusk's harvest reads the LP custody account this closes, so both
        // yield accounts must have nothing left to claim. Dusk only creates a
        // yield account for its market's assets, which binds both mints.
        for (yield_account, mint) in [
            (&a.base_yield_account, a.base_mint.key()),
            (&a.quote_yield_account, a.quote_mint.key()),
        ] {
            require!(
                yield_account.owner == order_key
                    && yield_account.market == a.order.market
                    && yield_account.lp_mint == a.order.lp_mint
                    && yield_account.asset_mint == mint
                    && (yield_account.token_kind == YieldTokenKind::Srlp.code()
                        || yield_account.token_kind == YieldTokenKind::Hlp.code()),
                LeverageDelegateError::InvalidOrder
            );
            require!(
                yield_account.accrued_swap_fee_amount == 0
                    && yield_account.accrued_interest_amount == 0
                    && yield_account
                        .income
                        .iter()
                        .all(|claim| claim.claimable == 0),
                LeverageDelegateError::UnclaimedYield
            );
        }

        let owner = a.owner.key();
        let id = a.order.order_id.to_le_bytes();
        let bump = [a.order.bump];
        let seeds = &[PROTECTION_ORDER_SEED_PREFIX, owner.as_ref(), &id, &bump];
        for (mint, custody, recipient) in [
            (&a.base_mint, &a.custody_base_account, &a.owner_base_account),
            (
                &a.quote_mint,
                &a.custody_quote_account,
                &a.owner_quote_account,
            ),
        ] {
            let Some(custody) = custody else {
                continue;
            };
            let token_program = token_program_for_mint(
                &mint.to_account_info(),
                &a.token_program.to_account_info(),
                &a.token_2022_program.to_account_info(),
            );
            // Anyone can transfer into custody; that balance goes to the sponsor.
            if custody.amount > 0 {
                let recipient = recipient
                    .as_ref()
                    .ok_or(LeverageDelegateError::InvalidTokenAccount)?;
                transfer_checked(
                    token_program.clone(),
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
            close_token_account_with_fee_harvest(
                token_program,
                custody.to_account_info(),
                mint.to_account_info(),
                a.owner.to_account_info(),
                a.order.to_account_info(),
                &[seeds],
            )?;
        }
        close_token_account(
            a.token_2022_program.to_account_info(),
            a.custody_lp_account.to_account_info(),
            a.owner.to_account_info(),
            a.order.to_account_info(),
            &[seeds],
        )
    }
}
