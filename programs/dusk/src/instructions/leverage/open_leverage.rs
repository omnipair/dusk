use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionOpened, MarketEventMetadata},
    shared::{
        account::get_size_with_discriminator,
        token::{create_token_account, transfer_from_user_to_vault, transfer_from_vault_to_vault},
    },
    state::{leverage_debt_from_margin, FutarchyAuthority, LeveragePosition, Market, MarketAsset},
};

use super::common::{
    leverage_collateral_credit, move_leverage_swap_fee, validate_leverage_fee_account, validate_leverage_mints,
    validate_leverage_reserve_accounts, validate_owner_debt_account,
};
use crate::instructions::common::{token_account_credit, token_program_for_mint};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct OpenLeverageArgs {
    pub position_id: Pubkey,
    pub debt_asset: u8,
    pub margin_amount: u64,
    pub multiplier_bps: u64,
    pub min_collateral_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: OpenLeverageArgs)]
pub struct OpenLeverage<'info> {
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

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        init,
        payer = owner,
        space = get_size_with_discriminator::<LeveragePosition>(),
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            args.position_id.as_ref(),
        ],
        bump
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
        bump
    )]
    /// CHECK: Created lazily with the collateral mint's token program.
    pub leverage_collateral_vault: UncheckedAccount<'info>,

    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> OpenLeverage<'info> {
    pub fn validate(&self, args: &OpenLeverageArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(args.margin_amount > 0, ErrorCode::AmountZero);
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        validate_leverage_mints(&self.market, debt_asset, &self.debt_mint, &self.collateral_mint)?;
        validate_leverage_reserve_accounts(
            &self.market,
            debt_asset,
            &self.debt_mint,
            &self.collateral_mint,
            &self.debt_reserve_vault,
            &self.collateral_reserve_vault,
        )?;
        validate_leverage_fee_account(&self.market, &self.debt_mint, &self.debt_fee_vault, debt_asset)?;
        validate_owner_debt_account(self.owner.key(), &self.debt_mint, &self.owner_debt_account)?;
        require_gte!(
            self.owner_debt_account.amount,
            args.margin_amount,
            ErrorCode::InsufficientBalance
        );
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(OpenLeverageArgs);

    pub fn handle_open(ctx: Context<'_, '_, '_, 'info, Self>, args: OpenLeverageArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();

        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let reserve_balance_before = ctx.accounts.debt_reserve_vault.amount;
        transfer_from_user_to_vault(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            args.margin_amount,
            ctx.accounts.debt_mint.decimals,
        )?;
        ctx.accounts.debt_reserve_vault.reload()?;
        let margin_credit = token_account_credit(reserve_balance_before, &ctx.accounts.debt_reserve_vault)?;
        require!(margin_credit > 0, ErrorCode::AmountZero);

        let debt_amount = leverage_debt_from_margin(margin_credit, args.multiplier_bps)?;
        let notional = margin_credit
            .checked_add(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let swap = ctx.accounts.market.quote_leverage_swap(debt_asset, notional)?;
        let collateral_credit = leverage_collateral_credit(&ctx.accounts.collateral_mint, swap.amount_out)?;
        require_gte!(collateral_credit, args.min_collateral_out, ErrorCode::SlippageExceeded);

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        create_token_account(
            &ctx.accounts.market.to_account_info(),
            &ctx.accounts.owner.to_account_info(),
            &ctx.accounts.leverage_collateral_vault.to_account_info(),
            &ctx.accounts.collateral_mint.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            &collateral_token_program,
            &[
                LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX,
                market_key.as_ref(),
                collateral_mint_key.as_ref(),
                &[ctx.bumps.leverage_collateral_vault],
            ],
        )?;

        move_leverage_swap_fee(
            &ctx.accounts.market,
            &ctx.accounts.debt_mint,
            &mut ctx.accounts.debt_reserve_vault,
            &mut ctx.accounts.debt_fee_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            swap.fee_credit,
        )?;
        transfer_from_vault_to_vault(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            swap.amount_out,
            ctx.accounts.collateral_mint.decimals,
            &[&crate::generate_market_seeds!(ctx.accounts.market)[..]],
        )?;

        let clock = Clock::get()?;
        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let receipt = ctx.accounts.market.open_leverage(
            &mut ctx.accounts.leverage_position,
            owner_key,
            market_key,
            args.position_id,
            debt_asset,
            margin_credit,
            args.multiplier_bps,
            collateral_credit,
            clock.unix_timestamp,
            clock.slot,
            ctx.bumps.leverage_position,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;

        emit_cpi!(LeveragePositionOpened {
            market: market_key,
            position: ctx.accounts.leverage_position.key(),
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            margin_amount: margin_credit,
            debt_amount: receipt.debt_amount,
            debt_shares: receipt.debt_shares,
            collateral_amount: receipt.collateral_amount,
            closeout_value: receipt.closeout_value,
            equity: receipt.equity,
            multiplier_bps: args.multiplier_bps,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}
