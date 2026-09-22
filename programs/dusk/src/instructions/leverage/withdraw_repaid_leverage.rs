use crate::{
    constants::*,
    errors::ErrorCode,
    events::{DebtFreePositionClosed, MarketEventMetadata},
    generate_market_seeds,
    instructions::accounts::{require_supported_asset_mint, token_program_for_mint},
    state::{LeveragePosition, Market},
    token::transfer_checked_with_remaining_accounts,
};
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct WithdrawRepaidLeverageArgs {
    pub min_collateral_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct WithdrawRepaidLeverage<'info> {
    #[account(mut, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut, close = owner, seeds = [LEVERAGE_POSITION_SEED_PREFIX, market.key().as_ref(), leverage_position.position_id.as_ref()], bump = leverage_position.bump,
        constraint = leverage_position.owner == owner.key() @ ErrorCode::InvalidSigner,
        constraint = leverage_position.market == market.key() @ ErrorCode::InvalidLeveragePosition)]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, seeds = [LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX, market.key().as_ref(), collateral_mint.key().as_ref()], bump,
        constraint = collateral_vault.owner == market.key() @ ErrorCode::InvalidVault,
        constraint = collateral_vault.mint == collateral_mint.key() @ ErrorCode::InvalidVault)]
    pub collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, constraint = owner_collateral_account.owner == owner.key() @ ErrorCode::InvalidTokenAccount,
        constraint = owner_collateral_account.mint == collateral_mint.key() @ ErrorCode::InvalidTokenAccount)]
    pub owner_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> WithdrawRepaidLeverage<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: WithdrawRepaidLeverageArgs) -> Result<()> {
        ctx.accounts.market.assert_started()?;
        require!(
            ctx.accounts.leverage_position.debt_shares == 0 && ctx.accounts.leverage_position.debt_principal == 0,
            ErrorCode::InsufficientDebt
        );
        let asset = ctx.accounts.leverage_position.collateral_asset()?;
        require_keys_eq!(
            ctx.accounts.market.side(asset).asset_mint,
            ctx.accounts.collateral_mint.key(),
            ErrorCode::InvalidMint
        );
        require_supported_asset_mint(&ctx.accounts.collateral_mint)?;
        let amount = ctx.accounts.leverage_position.collateral_amount;
        let balance_before = ctx.accounts.owner_collateral_account.amount;
        if amount > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.collateral_vault.to_account_info(),
                ctx.accounts.owner_collateral_account.to_account_info(),
                ctx.accounts.collateral_mint.to_account_info(),
                token_program_for_mint(
                    &ctx.accounts.collateral_mint,
                    &ctx.accounts.token_program,
                    &ctx.accounts.token_2022_program,
                )?,
                amount,
                ctx.accounts.collateral_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
        }
        ctx.accounts.owner_collateral_account.reload()?;
        let received = ctx
            .accounts
            .owner_collateral_account
            .amount
            .checked_sub(balance_before)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(received, args.min_collateral_out, ErrorCode::SlippageExceeded);
        emit_cpi!(DebtFreePositionClosed {
            market: ctx.accounts.market.key(),
            position: ctx.accounts.leverage_position.key(),
            owner: ctx.accounts.owner.key(),
            leverage: true,
            base_received: if asset == crate::state::MarketAsset::Base {
                received
            } else {
                0
            },
            quote_received: if asset == crate::state::MarketAsset::Quote {
                received
            } else {
                0
            },
            metadata: MarketEventMetadata::new(ctx.accounts.owner.key(), ctx.accounts.market.key())?,
        });
        Ok(())
    }
}
