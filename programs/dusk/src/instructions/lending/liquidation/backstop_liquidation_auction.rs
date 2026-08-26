use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use super::settlement::validate_liquidation_accounts;
use crate::{
    constants::*,
    errors::ErrorCode,
    events::BorrowPositionLiquidated,
    generate_market_seeds,
    instructions::{
        accounts::{
            require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
            validate_interest_accounts, HlpSwapAccountLayout,
        },
        enforce_launch_same_transaction_guard,
        referral::accounting::{
            accrue_referral_interest, referral_interest_accrued_event_at_slot, validate_referral_binding,
        },
        settle_inline_leverage_hlp, SwapRequest,
    },
    state::{BorrowPosition, FutarchyAuthority, Market, ReferralAccrual, ReferralPartner},
    token::transfer_checked_with_remaining_accounts,
    transitions::liquidity::SwapCashPolicy,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BackstopLiquidationAuctionArgs {
    /// Protects the keeper from a collateral transfer-fee change. Zero accepts
    /// any protocol-calculated bounty credit.
    pub min_caller_bounty_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: BackstopLiquidationAuctionArgs)]
pub struct BackstopLiquidationAuction<'info> {
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

    /// CHECK: Owner identity used to validate the residual token account.
    #[account(address = borrow_position.owner)]
    pub position_owner: AccountInfo<'info>,

    pub liquidator: Signer<'info>,

    pub debt_asset_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub liquidator_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

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

    /// CHECK: Canonical Instructions sysvar for the launch split guard.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> BackstopLiquidationAuction<'info> {
    pub fn validate(&self, _args: &BackstopLiquidationAuctionArgs) -> Result<()> {
        self.market.assert_started()?;
        let debt_asset = validate_liquidation_accounts(
            &self.market,
            &self.debt_asset_mint,
            &self.collateral_asset_mint,
            &self.debt_reserve_vault,
            &self.collateral_vault,
            &self.insurance_vault,
        )?;
        let collateral_side = self.market.side(debt_asset.opposite());
        require_keys_eq!(
            collateral_side.reserve_vault,
            self.collateral_reserve_vault.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.collateral_reserve_vault.mint,
            self.collateral_asset_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.collateral_reserve_vault.owner,
            self.market.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.liquidator_collateral_account.mint,
            self.collateral_asset_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(
            self.liquidator_collateral_account.owner,
            self.liquidator.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(
            self.owner_debt_account.mint,
            self.debt_asset_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(
            self.owner_debt_account.owner,
            self.position_owner.key(),
            ErrorCode::InvalidTokenAccount
        );
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

    crate::instructions::accounts::market_update_and_validate!(BackstopLiquidationAuctionArgs);

    pub fn handle_backstop(ctx: Context<'_, '_, '_, 'info, Self>, args: BackstopLiquidationAuctionArgs) -> Result<()> {
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let clock = Clock::get()?;
        let market_key = ctx.accounts.market.key();
        let borrow_position_key = ctx.accounts.borrow_position.key();
        let borrower_key = ctx.accounts.borrow_position.owner;
        let liquidator_key = ctx.accounts.liquidator.key();
        let debt_asset_mint_key = ctx.accounts.debt_asset_mint.key();
        let debt_asset = ctx.accounts.market.asset_for_mint(debt_asset_mint_key)?;
        let collateral_asset = debt_asset.opposite();
        let expected_referral_partner = ctx.accounts.borrow_position.referral_partner(debt_asset);
        let referral_interest_share_bps = ctx.accounts.borrow_position.referral_interest_share_bps(debt_asset);

        // Commit recovery-driven cancellation before checking expiry or moving
        // collateral. The initial assertion prevents an unrelated no-op call.
        ctx.accounts.borrow_position.assert_liquidation_auction(debt_asset)?;
        ctx.accounts
            .market
            .reconcile_liquidation_auction(&mut ctx.accounts.borrow_position)?;
        if !ctx.accounts.borrow_position.has_active_liquidation_auction() {
            return Ok(());
        }
        require!(
            ctx.accounts
                .borrow_position
                .liquidation_auction_expired(clock.unix_timestamp)?,
            ErrorCode::PositionNotLiquidatable
        );
        enforce_launch_same_transaction_guard(
            &ctx.accounts.market,
            market_key,
            collateral_asset,
            clock.unix_timestamp,
            &ctx.accounts.instructions_sysvar.to_account_info(),
        )?;

        let collateral_consumed = ctx.accounts.borrow_position.collateral(collateral_asset);
        require!(collateral_consumed > 0, ErrorCode::InsufficientBalance);
        let caller_bounty = u64::try_from(
            (collateral_consumed as u128)
                .checked_mul(LIQUIDATION_BACKSTOP_CALLER_BPS as u128)
                .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let collateral_for_swap = collateral_consumed
            .checked_sub(caller_bounty)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(collateral_for_swap > 0, ErrorCode::InsufficientAmount);

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let caller_balance_before = ctx.accounts.liquidator_collateral_account.amount;
        if caller_bounty > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.collateral_vault.to_account_info(),
                ctx.accounts.liquidator_collateral_account.to_account_info(),
                ctx.accounts.collateral_asset_mint.to_account_info(),
                collateral_token_program.clone(),
                caller_bounty,
                ctx.accounts.collateral_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
            )?;
            ctx.accounts.liquidator_collateral_account.reload()?;
        }
        let caller_bounty_credit =
            token_account_credit(caller_balance_before, &ctx.accounts.liquidator_collateral_account)?;
        require_gte!(
            caller_bounty_credit,
            args.min_caller_bounty_out,
            ErrorCode::SlippageExceeded
        );

        let collateral_reserve_before = ctx.accounts.collateral_reserve_vault.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.collateral_asset_mint.to_account_info(),
            collateral_token_program,
            collateral_for_swap,
            ctx.accounts.collateral_asset_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        let collateral_reserve_credit =
            token_account_credit(collateral_reserve_before, &ctx.accounts.collateral_reserve_vault)?;

        let protocol_fee_bps = ctx.accounts.futarchy_authority.revenue_share.swap_bps;
        let protocol_auction_split = ctx.accounts.futarchy_authority.protocol_auction_split;
        let prepared = if collateral_reserve_credit > 0 {
            Some(
                SwapRequest {
                    current_slot: clock.slot,
                    current_unix_timestamp: clock.unix_timestamp,
                    asset_in: collateral_asset,
                    reserve_credit: collateral_reserve_credit,
                    protocol_fee_bps,
                }
                .prepare_with_cash_policy(
                    &mut ctx.accounts.market,
                    SwapCashPolicy::Liquidate {
                        debt_asset,
                        debt_shares: 0,
                        debt_principal: 0,
                    },
                )?,
            )
        } else {
            None
        };
        let swap_output = prepared.as_ref().map(|swap| swap.quote.amount_out).unwrap_or(0);
        let full_repayment = ctx
            .accounts
            .market
            .fixed_repayment_for_max(&ctx.accounts.borrow_position, debt_asset, u64::MAX)?
            .cash_repaid;
        let repay_credit = swap_output.min(full_repayment);
        let remaining_debt = full_repayment
            .checked_sub(repay_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let insurance_request = if remaining_debt == 0 {
            0
        } else {
            ctx.accounts
                .market
                .insurance
                .draw_capacity(debt_asset, clock.slot)?
                .min(ctx.accounts.insurance_vault.amount)
                .min(remaining_debt)
        };
        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let (insurance_spent, insurance_credit) = if insurance_request > 0 {
            let insurance_balance_before = ctx.accounts.insurance_vault.amount;
            let reserve_balance_before = ctx.accounts.debt_reserve_vault.amount;
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.insurance_vault.to_account_info(),
                ctx.accounts.debt_reserve_vault.to_account_info(),
                ctx.accounts.debt_asset_mint.to_account_info(),
                debt_token_program.clone(),
                insurance_request,
                ctx.accounts.debt_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
            )?;
            ctx.accounts.insurance_vault.reload()?;
            ctx.accounts.debt_reserve_vault.reload()?;
            (
                insurance_balance_before
                    .checked_sub(ctx.accounts.insurance_vault.amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                token_account_credit(reserve_balance_before, &ctx.accounts.debt_reserve_vault)?,
            )
        } else {
            (0, 0)
        };

        let finalized = if let Some(prepared) = &prepared {
            Some(prepared.finalize_lending_liquidation_state(
                &mut ctx.accounts.market,
                clock.slot,
                protocol_fee_bps,
                protocol_auction_split,
            )?)
        } else {
            None
        };
        let liquidation = ctx.accounts.market.settle_internal_liquidation(
            &mut ctx.accounts.borrow_position,
            debt_asset,
            swap_output,
            insurance_spent,
            insurance_credit,
            collateral_consumed,
            caller_bounty,
        )?;
        let liquidation_receipt = liquidation.liquidation;
        if liquidation_receipt.socialized_loss > 0 {
            ctx.accounts
                .market
                .finalize_amm_socialized_loss_and_observe_risk(clock.slot)?;
        } else {
            ctx.accounts.market.finalize_amm_transition(clock.slot)?;
            ctx.accounts.market.refresh_risk()?;
        }

        if let (Some(prepared), Some(finalized)) = (&prepared, finalized) {
            settle_inline_leverage_hlp(
                &mut ctx.accounts.market,
                &ctx.accounts.futarchy_authority,
                debt_asset,
                &ctx.accounts.debt_asset_mint,
                &ctx.accounts.collateral_asset_mint,
                &ctx.accounts.debt_reserve_vault,
                &ctx.accounts.collateral_reserve_vault,
                &ctx.accounts.token_program,
                &ctx.accounts.token_2022_program,
                ctx.remaining_accounts,
                h_lp_accounts,
                finalized.base_rebalance,
                finalized.quote_rebalance,
                prepared.interest_eligibility,
            )?;
        }
        ctx.accounts.interest_vault.reload()?;

        let interest_vault_balance_before = ctx.accounts.interest_vault.amount;
        if liquidation_receipt.interest_paid > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.debt_reserve_vault.to_account_info(),
                ctx.accounts.interest_vault.to_account_info(),
                ctx.accounts.debt_asset_mint.to_account_info(),
                debt_token_program.clone(),
                liquidation_receipt.interest_paid,
                ctx.accounts.debt_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
            )?;
            ctx.accounts.interest_vault.reload()?;
        }
        let interest_vault_credit = token_account_credit(interest_vault_balance_before, &ctx.accounts.interest_vault)?;
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
            protocol_auction_split,
            referral_receipt.quote.referral_amount,
        )?;

        if liquidation.owner_residual > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.debt_reserve_vault.to_account_info(),
                ctx.accounts.owner_debt_account.to_account_info(),
                ctx.accounts.debt_asset_mint.to_account_info(),
                debt_token_program,
                liquidation.owner_residual,
                ctx.accounts.debt_asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
            )?;
        }

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

        emit_cpi!(BorrowPositionLiquidated {
            market: market_key,
            borrow_position: borrow_position_key,
            borrower: borrower_key,
            liquidator: liquidator_key,
            debt_asset_side: debt_asset.code(),
            repaid_amount: liquidation_receipt.repaid_amount,
            collateral_seized: liquidation_receipt.collateral_seized,
            collateral_to_liquidator: liquidation_receipt.collateral_to_liquidator,
            collateral_credit: caller_bounty_credit,
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
            clock.slot,
        )? {
            emit_cpi!(event);
        }
        Ok(())
    }
}
