use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionLiquidated, LeverageSwapEvent, MarketEventMetadata},
    generate_market_seeds,
    shared::token::transfer_checked_with_remaining_accounts,
    state::{
        FutarchyAuthority, LeveragePosition, LeverageSwapPlan, LeverageSwapQuote, Market, MarketAsset, ReferralAccrual,
        ReferralPartner,
    },
};

use super::common::{
    leverage_swap_fee_credit, record_leverage_interest, settle_inline_leverage_hlp, validate_leverage_futarchy_pda,
    validate_leverage_interest_account, validate_leverage_market_pda, validate_leverage_mints,
    validate_leverage_reserve_accounts,
};
use crate::instructions::common::{
    require_reserve_custody, token_account_credit, token_program_for_mint, HlpSwapAccountLayout,
};
use crate::instructions::referral::common::{emit_referral_interest_accrued_at_slot, validate_referral_binding};
use crate::instructions::{SwapContext, SwapPlan};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct LiquidateLeverageArgs {
    pub debt_asset: u8,
}

#[derive(Accounts)]
#[instruction(args: LiquidateLeverageArgs)]
pub struct LiquidateLeverage<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    /// CHECK: Receives closed account rent and any non-incentive residual.
    #[account(mut, address = leverage_position.owner)]
    pub position_owner: AccountInfo<'info>,

    #[account(
        mut,
        close = position_owner,
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
    pub debt_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,

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

    #[account(
        mut,
        constraint = liquidator_debt_account.mint == debt_mint.key() @ ErrorCode::InvalidTokenAccount,
        constraint = liquidator_debt_account.owner == liquidator.key() @ ErrorCode::InvalidTokenAccount,
    )]
    pub liquidator_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = owner_debt_account.mint == debt_mint.key() @ ErrorCode::InvalidTokenAccount,
        constraint = owner_debt_account.owner == position_owner.key() @ ErrorCode::InvalidTokenAccount,
    )]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,

    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,

    #[account(mut)]
    pub liquidator: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> LiquidateLeverage<'info> {
    pub fn validate_at(&self, args: &LiquidateLeverageArgs, unix_timestamp: i64) -> Result<()> {
        validate_leverage_market_pda(&self.market, self.market.key())?;
        validate_leverage_futarchy_pda(self.futarchy_authority.bump, self.futarchy_authority.key())?;
        self.market.assert_started_at(unix_timestamp)?;
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
        validate_leverage_interest_account(&self.market, &self.debt_mint, &self.debt_interest_vault, debt_asset)?;
        self.leverage_position.require_open()?;
        validate_referral_binding(
            None,
            self.leverage_position.referral_partner,
            self.leverage_position.referral_interest_share_bps,
            true,
            &self.futarchy_authority,
            self.referral_partner.as_deref(),
            self.referral_accrual.as_deref(),
            self.market.key(),
            &self.debt_mint,
        )?;
        Ok(())
    }

    pub fn handle_liquidate(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: LiquidateLeverageArgs,
        current_slot: u64,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let liquidator_key = ctx.accounts.liquidator.key();
        let owner_key = ctx.accounts.position_owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let position_key = ctx.accounts.leverage_position.key();
        let expected_referral_partner = ctx.accounts.leverage_position.referral_partner;
        let collateral_sold = ctx.accounts.leverage_position.collateral_amount;

        // Return seized collateral to the reserve and measure its net credit.
        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let collateral_reserve_balance_before = ctx.accounts.collateral_reserve_vault.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            collateral_sold,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        let collateral_reserve_credit = token_account_credit(
            collateral_reserve_balance_before,
            &ctx.accounts.collateral_reserve_vault,
        )?;
        require!(collateral_reserve_credit > 0, ErrorCode::AmountZero);

        // Quote the credited collateral as a debt-asset liquidation swap.
        let SwapPlan {
            quote,
            base_pre_rebalance,
            quote_pre_rebalance,
            fee_eligible_ylp_supply,
            interest_eligibility,
        } = SwapContext {
            current_slot,
            asset_in: collateral_asset,
            reserve_credit: collateral_reserve_credit,
        }
        .plan(&mut ctx.accounts.market)?;
        ctx.accounts.market.observe_current_risk(current_slot)?;
        let swap = LeverageSwapQuote::from_amm(quote, current_slot);
        let swap_plan = LeverageSwapPlan {
            swap,
            base_pre_rebalance,
            quote_pre_rebalance,
            fee_eligible_ylp_supply,
            interest_eligibility,
        };
        let swap_fee_credit = leverage_swap_fee_credit(&swap)?;

        // Commit liquidation accounting and settle the resulting hLP exposure.
        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let receipt = ctx.accounts.market.liquidate_leverage(
            &mut ctx.accounts.leverage_position,
            swap_plan,
            swap_fee_credit,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            current_slot,
        )?;
        settle_inline_leverage_hlp(
            &mut ctx.accounts.market,
            &ctx.accounts.futarchy_authority,
            debt_asset,
            &ctx.accounts.debt_mint,
            &ctx.accounts.collateral_mint,
            &ctx.accounts.debt_reserve_vault,
            &ctx.accounts.collateral_reserve_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            ctx.remaining_accounts,
            h_lp_accounts,
            receipt.base_hlp_rebalance,
            receipt.quote_hlp_rebalance,
            interest_eligibility,
        )?;

        // Pay the liquidator first, then return any residual to the owner.
        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let liquidator_balance_before = ctx.accounts.liquidator_debt_account.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.liquidator_debt_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program.clone(),
            receipt.liquidator_amount,
            ctx.accounts.debt_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.liquidator_debt_account.reload()?;
        let liquidator_amount = token_account_credit(liquidator_balance_before, &ctx.accounts.liquidator_debt_account)?;

        let owner_balance_before = ctx.accounts.owner_debt_account.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            receipt.owner_residual,
            ctx.accounts.debt_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.owner_debt_account.reload()?;
        let owner_residual = token_account_credit(owner_balance_before, &ctx.accounts.owner_debt_account)?;

        // Route accrued interest and reconcile physical reserve custody.
        let referral_receipt = record_leverage_interest(
            &mut ctx.accounts.market,
            debt_asset,
            &ctx.accounts.debt_mint,
            &mut ctx.accounts.debt_reserve_vault,
            &mut ctx.accounts.debt_interest_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            manager_fee_bps,
            &ctx.accounts.futarchy_authority,
            expected_referral_partner,
            ctx.accounts.leverage_position.referral_interest_share_bps,
            ctx.accounts.referral_partner.as_deref(),
            ctx.accounts.referral_accrual.as_deref_mut(),
            receipt.interest_paid,
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.debt_reserve_vault.reload()?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        require_reserve_custody(
            ctx.accounts.debt_reserve_vault.amount,
            ctx.accounts.market.side(debt_asset),
        )?;
        require_reserve_custody(
            ctx.accounts.collateral_reserve_vault.amount,
            ctx.accounts.market.side(collateral_asset),
        )?;

        emit_referral_interest_accrued_at_slot(
            &referral_receipt,
            market_key,
            position_key,
            owner_key,
            liquidator_key,
            debt_mint_key,
            current_slot,
        )?;

        // Emit the final liquidation state.
        emit!(LeveragePositionLiquidated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            liquidator: liquidator_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            debt_repaid: receipt.debt_repaid,
            interest_paid: receipt.interest_paid,
            principal_written_off: receipt.principal_written_off,
            collateral_sold: receipt.collateral_sold,
            closeout_value: receipt.closeout_value,
            liquidator_amount,
            owner_residual,
            swap: LeverageSwapEvent::new(receipt.swap, swap_fee_credit),
            metadata: MarketEventMetadata::at_slot(liquidator_key, market_key, current_slot),
        });
        Ok(())
    }
}
