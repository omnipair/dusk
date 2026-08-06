use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use super::common::{reconcile_insurance_funding_credit, validate_liquidation_accounts};
use crate::{
    constants::*,
    errors::ErrorCode,
    events::PositionLiquidated,
    generate_market_seeds,
    instructions::{
        common::{
            require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
            validate_interest_accounts,
        },
        referral::common::{
            accrue_referral_interest, referral_interest_accrued_event_at_slot, validate_referral_binding,
        },
    },
    shared::token::{get_transfer_fee, get_transfer_inverse_fee, transfer_checked_with_remaining_accounts},
    state::{
        market::transitions::liquidation::LiquidationPricing, BorrowPosition, FutarchyAuthority, Market,
        ReferralAccrual, ReferralPartner,
    },
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SettleLiquidationAuctionFloorArgs {
    pub repay_amount: u64,
    pub min_collateral_out: u64,
    pub max_insurance_draw: u64,
    pub max_socialized_loss: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: SettleLiquidationAuctionFloorArgs)]
pub struct SettleLiquidationAuctionFloor<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
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
    pub liquidator: Signer<'info>,

    pub debt_asset_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub liquidator_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub liquidator_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            BORROW_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            borrow_position.position_id.as_ref(),
        ],
        bump = borrow_position.bump
    )]
    pub borrow_position: Box<Account<'info, BorrowPosition>>,

    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,

    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> SettleLiquidationAuctionFloor<'info> {
    pub fn validate(&self, args: &SettleLiquidationAuctionFloorArgs) -> Result<()> {
        self.market.assert_started()?;
        require!(args.repay_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            self.liquidator_debt_account.amount,
            args.repay_amount,
            ErrorCode::InsufficientBalance
        );
        let debt_asset = validate_liquidation_accounts(
            &self.market,
            self.liquidator.key(),
            &self.debt_asset_mint,
            &self.collateral_asset_mint,
            &self.reserve_vault,
            &self.collateral_vault,
            &self.insurance_vault,
            &self.collateral_insurance_vault,
            &self.liquidator_debt_account,
            &self.liquidator_collateral_account,
        )?;
        let interest_asset = validate_interest_accounts(&self.market, &self.debt_asset_mint, &self.interest_vault)?;
        require!(interest_asset == debt_asset, ErrorCode::InvalidVault);
        require_supported_asset_mint(&self.debt_asset_mint)?;
        require_supported_asset_mint(&self.collateral_asset_mint)?;
        require_keys_eq!(
            self.borrow_position.market,
            self.market.key(),
            ErrorCode::InvalidBorrowPosition
        );
        validate_referral_binding(
            None,
            self.borrow_position.referral_partner(debt_asset),
            self.borrow_position.referral_interest_share_bps(debt_asset),
            true,
            &self.futarchy_authority,
            self.referral_partner.as_deref(),
            self.referral_accrual.as_deref(),
            self.market.key(),
            &self.debt_asset_mint,
        )?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(SettleLiquidationAuctionFloorArgs);

    pub fn handle_settle(ctx: Context<'_, '_, '_, 'info, Self>, args: SettleLiquidationAuctionFloorArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let borrow_position_key = ctx.accounts.borrow_position.key();
        let borrower_key = ctx.accounts.borrow_position.owner;
        let liquidator_key = ctx.accounts.liquidator.key();
        let debt_asset_mint_key = ctx.accounts.debt_asset_mint.key();
        let debt_asset = ctx.accounts.market.asset_for_mint(debt_asset_mint_key)?;
        let expected_referral_partner = ctx.accounts.borrow_position.referral_partner(debt_asset);
        let referral_interest_share_bps = ctx.accounts.borrow_position.referral_interest_share_bps(debt_asset);

        // Reconciliation must precede the floor-price read and every token CPI;
        // an auction does not lock liquidation after current risk recovers.
        ctx.accounts
            .market
            .reconcile_liquidation_auction(&mut ctx.accounts.borrow_position)?;
        ctx.accounts.borrow_position.assert_liquidation_auction(debt_asset)?;
        let now = Clock::get()?.unix_timestamp;
        let elapsed_s = now.saturating_sub(ctx.accounts.borrow_position.auction_start_time);
        require!(elapsed_s >= 0, ErrorCode::MarketMathOverflow);
        let elapsed_ms = (elapsed_s as u64).saturating_mul(1000);

        let decayed_price = crate::math::risk::exponential_price_decay(
            ctx.accounts.borrow_position.auction_start_price_nad,
            elapsed_ms,
            300_000,
        )?;

        let floor_price = ctx.accounts.borrow_position.auction_floor_price_nad;
        // Floor settlement starts only after the auction reaches its stored floor.
        require!(decayed_price <= floor_price, ErrorCode::PositionNotLiquidatable);

        let liquidation_pricing = LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad: floor_price,
        };
        let liquidation_terms = ctx.accounts.market.liquidation_terms_with_pricing(
            &ctx.accounts.borrow_position,
            debt_asset,
            liquidation_pricing,
        )?;
        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let max_repay_credit = args
            .repay_amount
            .checked_sub(get_transfer_fee(
                &ctx.accounts.debt_asset_mint.to_account_info(),
                args.repay_amount,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let full_aggregate_repayment = ctx
            .accounts
            .market
            .fixed_repayment_for_max(&ctx.accounts.borrow_position, debt_asset, u64::MAX)?
            .cash_repaid;
        let repay_credit = ctx
            .accounts
            .market
            .fixed_repayment_for_max(&ctx.accounts.borrow_position, debt_asset, max_repay_credit)?
            .cash_repaid;
        require_gte!(
            liquidation_terms.max_repay_amount,
            repay_credit,
            ErrorCode::LiquidationRepayTooLarge
        );
        let repay_gross = repay_credit
            .checked_add(get_transfer_inverse_fee(
                &ctx.accounts.debt_asset_mint.to_account_info(),
                repay_credit,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(args.repay_amount, repay_gross, ErrorCode::BrokenInvariant);
        let reserve_balance_before_repay = ctx.accounts.reserve_vault.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.liquidator.to_account_info(),
            ctx.accounts.liquidator_debt_account.to_account_info(),
            ctx.accounts.reserve_vault.to_account_info(),
            ctx.accounts.debt_asset_mint.to_account_info(),
            debt_token_program.clone(),
            repay_gross,
            ctx.accounts.debt_asset_mint.decimals,
            &[],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.reserve_vault.reload()?;
        require_eq!(
            token_account_credit(reserve_balance_before_repay, &ctx.accounts.reserve_vault)?,
            repay_credit,
            ErrorCode::BrokenInvariant
        );

        let insurance_request = if args.max_insurance_draw > 0 {
            ctx.accounts
                .market
                .insurance_request_for_liquidation_with_terms_and_pricing(
                    &ctx.accounts.borrow_position,
                    debt_asset,
                    repay_credit,
                    args.max_insurance_draw,
                    liquidation_terms,
                    liquidation_pricing,
                )?
                .min(
                    full_aggregate_repayment
                        .checked_sub(repay_credit)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                )
        } else {
            0
        };
        let (insurance_spent, insurance_credit) = if insurance_request > 0 {
            let reserve_balance_before_insurance = ctx.accounts.reserve_vault.amount;
            let insurance_balance_before = ctx.accounts.insurance_vault.amount;
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.insurance_vault.to_account_info(),
                ctx.accounts.reserve_vault.to_account_info(),
                ctx.accounts.debt_asset_mint.to_account_info(),
                debt_token_program.clone(),
                insurance_request,
                ctx.accounts.debt_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.reserve_vault.reload()?;
            ctx.accounts.insurance_vault.reload()?;
            (
                insurance_balance_before
                    .checked_sub(ctx.accounts.insurance_vault.amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                ctx.accounts
                    .reserve_vault
                    .amount
                    .checked_sub(reserve_balance_before_insurance)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            )
        } else {
            (0, 0)
        };

        let liquidation_receipt = ctx.accounts.market.settle_liquidation(
            &mut ctx.accounts.borrow_position,
            debt_asset,
            repay_credit,
            insurance_spent,
            insurance_credit,
            args.max_socialized_loss,
            liquidation_terms,
            liquidation_pricing,
        )?;
        let referral_receipt = if liquidation_receipt.interest_paid > 0 {
            let interest_vault_balance_before = ctx.accounts.interest_vault.amount;
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.reserve_vault.to_account_info(),
                ctx.accounts.interest_vault.to_account_info(),
                ctx.accounts.debt_asset_mint.to_account_info(),
                debt_token_program,
                liquidation_receipt.interest_paid,
                ctx.accounts.debt_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.reserve_vault.reload()?;
            ctx.accounts.interest_vault.reload()?;
            let interest_vault_credit =
                token_account_credit(interest_vault_balance_before, &ctx.accounts.interest_vault)?;
            let referral_receipt = accrue_referral_interest(
                expected_referral_partner,
                referral_interest_share_bps,
                &ctx.accounts.futarchy_authority,
                ctx.accounts.referral_partner.as_deref(),
                ctx.accounts.referral_accrual.as_deref_mut(),
                market_key,
                &ctx.accounts.debt_asset_mint,
                liquidation_receipt.interest_paid,
                interest_vault_credit,
                ctx.accounts.futarchy_authority.revenue_share.interest_bps,
            )?;
            ctx.accounts.market.side_mut(debt_asset).record_interest_credit(
                interest_vault_credit,
                ctx.accounts.futarchy_authority.revenue_share.interest_bps,
                ctx.accounts.futarchy_authority.protocol_auction_split,
                referral_receipt.quote.referral_amount,
            )?;
            referral_receipt
        } else {
            accrue_referral_interest(
                expected_referral_partner,
                referral_interest_share_bps,
                &ctx.accounts.futarchy_authority,
                ctx.accounts.referral_partner.as_deref(),
                ctx.accounts.referral_accrual.as_deref_mut(),
                market_key,
                &ctx.accounts.debt_asset_mint,
                0,
                0,
                ctx.accounts.futarchy_authority.revenue_share.interest_bps,
            )?
        };

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let collateral_credit = if liquidation_receipt.collateral_to_liquidator > 0 {
            let transfer_fee = get_transfer_fee(
                &ctx.accounts.collateral_asset_mint.to_account_info(),
                liquidation_receipt.collateral_to_liquidator,
            )?;
            let collateral_credit = liquidation_receipt
                .collateral_to_liquidator
                .checked_sub(transfer_fee)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require_gte!(collateral_credit, args.min_collateral_out, ErrorCode::SlippageExceeded);
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.collateral_vault.to_account_info(),
                ctx.accounts.liquidator_collateral_account.to_account_info(),
                ctx.accounts.collateral_asset_mint.to_account_info(),
                collateral_token_program.clone(),
                liquidation_receipt.collateral_to_liquidator,
                ctx.accounts.collateral_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            collateral_credit
        } else {
            0
        };
        require_gte!(collateral_credit, args.min_collateral_out, ErrorCode::SlippageExceeded);
        if liquidation_receipt.insurance_funded > 0 {
            let collateral_insurance_balance_before = ctx.accounts.collateral_insurance_vault.amount;
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.collateral_vault.to_account_info(),
                ctx.accounts.collateral_insurance_vault.to_account_info(),
                ctx.accounts.collateral_asset_mint.to_account_info(),
                collateral_token_program,
                liquidation_receipt.insurance_funded,
                ctx.accounts.collateral_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.collateral_insurance_vault.reload()?;
            let insurance_credit = crate::instructions::common::token_account_credit(
                collateral_insurance_balance_before,
                &ctx.accounts.collateral_insurance_vault,
            )?;
            reconcile_insurance_funding_credit(
                &mut ctx.accounts.market.insurance,
                debt_asset,
                liquidation_receipt.insurance_funded,
                insurance_credit,
            )?;
        }

        let current_slot = Clock::get()?.slot;
        if liquidation_receipt.socialized_loss > 0 {
            ctx.accounts.market.checkpoint_amm_socialized_loss_raw(current_slot)?;
            let parameters = ctx.accounts.market.amm.applied_curve_parameters;
            if ctx.accounts.market.amm.ramp.active
                || (!parameters.is_cpmm() && ctx.accounts.market.config.amm.adjustment_step_nad > 0)
            {
                ctx.accounts.market.amm.mark_retention_target_stale();
            } else {
                let q_per_share_nad = ctx.accounts.market.amm.q_per_share_nad;
                ctx.accounts.market.amm.refresh_retention_target(q_per_share_nad, 0)?;
            }
        } else {
            ctx.accounts.market.finalize_amm_transition(current_slot)?;
        }
        ctx.accounts.market.refresh_risk()?;
        require_reserve_custody(ctx.accounts.reserve_vault.amount, ctx.accounts.market.side(debt_asset))?;

        emit_cpi!(PositionLiquidated {
            market: market_key,
            borrow_position: borrow_position_key,
            borrower: borrower_key,
            liquidator: liquidator_key,
            debt_asset_side: debt_asset.code(),
            repaid_amount: liquidation_receipt.repaid_amount,
            collateral_seized: liquidation_receipt.collateral_seized,
            collateral_to_liquidator: liquidation_receipt.collateral_to_liquidator,
            collateral_credit,
            insurance_drawn: liquidation_receipt.insurance_drawn,
            socialized_loss: liquidation_receipt.socialized_loss,
            remaining_debt: liquidation_receipt.remaining_debt,
        });
        if let Some(event) = referral_interest_accrued_event_at_slot(
            &referral_receipt,
            market_key,
            borrow_position_key,
            borrower_key,
            liquidator_key,
            debt_asset_mint_key,
            current_slot,
        )? {
            emit_cpi!(event);
        }
        Ok(())
    }
}
