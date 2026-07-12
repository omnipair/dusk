use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionUpdated, MarketEventMetadata},
    generate_market_seeds,
    shared::token::transfer_from_vault_to_vault,
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset},
};

use super::common::{
    leverage_collateral_credit, move_leverage_swap_fee, validate_leverage_fee_account,
    validate_leverage_mints, validate_leverage_reserve_accounts,
};
use crate::instructions::common::token_program_for_mint;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct IncreaseLeverageArgs {
    pub debt_asset: u8,
    pub debt_amount: u64,
    pub min_collateral_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: IncreaseLeverageArgs)]
pub struct IncreaseLeverage<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_mint.as_ref(),
            market.quote_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(address = leverage_position.owner)]
    /// CHECK: Owner address bound by leverage_position.
    pub position_owner: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            leverage_position.position_id.as_ref(),
        ],
        bump = leverage_position.bump,
        constraint = leverage_position.market == market.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.debt_asset == args.debt_asset @ ErrorCode::InvalidLeveragePosition,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,

    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub debt_fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            collateral_mint.key().as_ref(),
        ],
        bump,
        constraint = leverage_collateral_vault.mint == collateral_mint.key() @ ErrorCode::InvalidVault,
        constraint = leverage_collateral_vault.owner == market.key() @ ErrorCode::InvalidVault
    )]
    pub leverage_collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> IncreaseLeverage<'info> {
    pub fn validate(&self, args: &IncreaseLeverageArgs) -> Result<()> {
        self.market
            .assert_live_with_futarchy(&self.futarchy_authority)?;
        require_keys_eq!(
            self.owner.key(),
            self.position_owner.key(),
            ErrorCode::InvalidSigner
        );
        require!(args.debt_amount > 0, ErrorCode::AmountZero);
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        validate_leverage_mints(
            &self.market,
            debt_asset,
            &self.debt_mint,
            &self.collateral_mint,
        )?;
        validate_leverage_reserve_accounts(
            &self.market,
            debt_asset,
            &self.debt_mint,
            &self.collateral_mint,
            &self.debt_reserve_vault,
            &self.collateral_reserve_vault,
        )?;
        validate_leverage_fee_account(
            &self.market,
            &self.debt_mint,
            &self.debt_fee_vault,
            debt_asset,
        )?;
        self.leverage_position.require_open()?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(IncreaseLeverageArgs);

    pub fn handle_increase(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: IncreaseLeverageArgs,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let position_key = ctx.accounts.leverage_position.key();

        let swap = ctx
            .accounts
            .market
            .quote_leverage_swap(debt_asset, args.debt_amount)?;
        let collateral_credit =
            leverage_collateral_credit(&ctx.accounts.collateral_mint, swap.amount_out)?;
        require_gte!(
            collateral_credit,
            args.min_collateral_out,
            ErrorCode::SlippageExceeded
        );

        move_leverage_swap_fee(
            &ctx.accounts.market,
            &ctx.accounts.debt_mint,
            &mut ctx.accounts.debt_reserve_vault,
            &mut ctx.accounts.debt_fee_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            swap.fee_credit,
        )?;
        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_from_vault_to_vault(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            swap.amount_out,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;

        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let receipt = ctx.accounts.market.increase_leverage(
            &mut ctx.accounts.leverage_position,
            args.debt_amount,
            collateral_credit,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;

        emit_cpi!(LeveragePositionUpdated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            debt_delta: receipt.debt_delta,
            collateral_delta: receipt.collateral_delta,
            debt_amount: receipt.debt_amount,
            debt_shares: receipt.debt_shares,
            collateral_amount: receipt.collateral_amount,
            closeout_value: receipt.closeout_value,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}
