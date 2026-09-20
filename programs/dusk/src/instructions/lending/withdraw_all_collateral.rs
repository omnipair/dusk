use crate::{
    constants::*,
    errors::ErrorCode,
    events::{DebtFreePositionClosed, MarketEventMetadata},
    generate_market_seeds,
    instructions::accounts::{require_supported_asset_mint, token_program_for_mint},
    state::{BorrowPosition, Market, MarketAsset},
    token::transfer_checked_with_remaining_accounts,
};
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct WithdrawAllCollateralArgs {
    pub min_base_out: u64,
    pub min_quote_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct WithdrawAllCollateral<'info> {
    #[account(mut, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut, close = owner, seeds = [BORROW_POSITION_SEED_PREFIX, market.key().as_ref(), borrow_position.position_id.as_ref()], bump = borrow_position.bump,
        constraint = borrow_position.owner == owner.key() @ ErrorCode::InvalidSigner,
        constraint = borrow_position.market == market.key() @ ErrorCode::InvalidBorrowPosition)]
    pub borrow_position: Box<Account<'info, BorrowPosition>>,
    #[account(address = market.base_side.asset_mint)]
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address = market.quote_side.asset_mint)]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, address = market.base_side.collateral_vault, constraint = base_collateral_vault.owner == market.key() @ ErrorCode::InvalidVault, constraint = base_collateral_vault.mint == base_mint.key() @ ErrorCode::InvalidVault)]
    pub base_collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, address = market.quote_side.collateral_vault, constraint = quote_collateral_vault.owner == market.key() @ ErrorCode::InvalidVault, constraint = quote_collateral_vault.mint == quote_mint.key() @ ErrorCode::InvalidVault)]
    pub quote_collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = owner_base_account.owner == owner.key() @ ErrorCode::InvalidTokenAccount, constraint = owner_base_account.mint == base_mint.key() @ ErrorCode::InvalidTokenAccount)]
    pub owner_base_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = owner_quote_account.owner == owner.key() @ ErrorCode::InvalidTokenAccount, constraint = owner_quote_account.mint == quote_mint.key() @ ErrorCode::InvalidTokenAccount)]
    pub owner_quote_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> WithdrawAllCollateral<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: WithdrawAllCollateralArgs) -> Result<()> {
        ctx.accounts.market.assert_started()?;
        require!(
            ctx.accounts.borrow_position.fixed_base_shares == 0 && ctx.accounts.borrow_position.fixed_quote_shares == 0,
            ErrorCode::InsufficientDebt
        );
        let base_before = ctx.accounts.owner_base_account.amount;
        let quote_before = ctx.accounts.owner_quote_account.amount;
        for (asset, mint, vault, destination) in [
            (
                MarketAsset::Base,
                &ctx.accounts.base_mint,
                &ctx.accounts.base_collateral_vault,
                &ctx.accounts.owner_base_account,
            ),
            (
                MarketAsset::Quote,
                &ctx.accounts.quote_mint,
                &ctx.accounts.quote_collateral_vault,
                &ctx.accounts.owner_quote_account,
            ),
        ] {
            require_supported_asset_mint(mint)?;
            let amount = ctx.accounts.borrow_position.collateral(asset);
            if amount > 0 {
                transfer_checked_with_remaining_accounts(
                    ctx.accounts.market.to_account_info(),
                    vault.to_account_info(),
                    destination.to_account_info(),
                    mint.to_account_info(),
                    token_program_for_mint(mint, &ctx.accounts.token_program, &ctx.accounts.token_2022_program)?,
                    amount,
                    mint.decimals,
                    &[&generate_market_seeds!(ctx.accounts.market)[..]],
                    ctx.remaining_accounts,
                )?;
                ctx.accounts
                    .market
                    .withdraw_collateral(&mut ctx.accounts.borrow_position, asset, amount, 0)?;
            }
        }
        require!(ctx.accounts.borrow_position.is_empty(), ErrorCode::BrokenInvariant);
        ctx.accounts.owner_base_account.reload()?;
        ctx.accounts.owner_quote_account.reload()?;
        let base_received = ctx
            .accounts
            .owner_base_account
            .amount
            .checked_sub(base_before)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let quote_received = ctx
            .accounts
            .owner_quote_account
            .amount
            .checked_sub(quote_before)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(base_received, args.min_base_out, ErrorCode::SlippageExceeded);
        require_gte!(quote_received, args.min_quote_out, ErrorCode::SlippageExceeded);
        emit_cpi!(DebtFreePositionClosed {
            market: ctx.accounts.market.key(),
            position: ctx.accounts.borrow_position.key(),
            owner: ctx.accounts.owner.key(),
            leverage: false,
            base_received,
            quote_received,
            metadata: MarketEventMetadata::new(ctx.accounts.owner.key(), ctx.accounts.market.key())?
        });
        Ok(())
    }
}
