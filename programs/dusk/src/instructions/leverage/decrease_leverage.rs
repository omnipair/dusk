use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionUpdated, LeverageSwapEvent, MarketEventMetadata},
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
pub struct DecreaseLeverageArgs {
    pub debt_asset: u8,
    pub collateral_amount: u64,
    pub min_repay_out: u64,
}

#[derive(Accounts)]
#[instruction(args: DecreaseLeverageArgs)]
pub struct DecreaseLeverage<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

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

    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,

    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,

    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> DecreaseLeverage<'info> {
    pub fn validate_at(&self, args: &DecreaseLeverageArgs, unix_timestamp: i64) -> Result<()> {
        validate_leverage_market_pda(&self.market, self.market.key())?;
        validate_leverage_futarchy_pda(self.futarchy_authority.bump, self.futarchy_authority.key())?;
        self.market.assert_started_at(unix_timestamp)?;
        require_keys_eq!(self.owner.key(), self.position_owner.key(), ErrorCode::InvalidSigner);
        require!(args.collateral_amount > 0, ErrorCode::AmountZero);
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

    pub fn handle_decrease(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: DecreaseLeverageArgs,
        current_slot: u64,
    ) -> Result<()> {
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let position_key = ctx.accounts.leverage_position.key();

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
            args.collateral_amount,
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

        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let receipt = ctx.accounts.market.decrease_leverage(
            &mut ctx.accounts.leverage_position,
            args.collateral_amount,
            args.min_repay_out,
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
            ctx.accounts.leverage_position.referral_partner,
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
            owner_key,
            debt_mint_key,
            current_slot,
        )?;

        emit!(LeveragePositionUpdated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            borrowed_amount: receipt.borrowed_amount,
            debt_delta: receipt.debt_delta,
            collateral_delta: receipt.collateral_delta,
            debt_amount: receipt.debt_amount,
            debt_shares: receipt.debt_shares,
            collateral_amount: receipt.collateral_amount,
            closeout_value: receipt.closeout_value,
            swap: Some(LeverageSwapEvent::new(swap, swap_fee_credit)),
            metadata: MarketEventMetadata::at_slot(owner_key, market_key, current_slot),
        });
        Ok(())
    }
}
