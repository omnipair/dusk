use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketDebtUpdated, MarketEventMetadata, MarketHealthUpdated},
    generate_market_seeds,
    shared::token::{get_transfer_fee, get_transfer_inverse_fee, transfer_checked_with_remaining_accounts},
    state::{BorrowPosition, FutarchyAuthority, Market, ReferralAccrual, ReferralPartner},
};

use crate::instructions::common::{
    require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
    validate_interest_accounts,
};

use super::common::validate_debt_reserve_accounts;
use crate::instructions::referral::common::{
    accrue_referral_interest, referral_interest_accrued_event_at_slot, validate_referral_binding,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct RepayArgs {
    pub repay_amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: RepayArgs)]
pub struct Repay<'info> {
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
    pub owner: Signer<'info>,

    pub debt_asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,

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

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> Repay<'info> {
    pub fn validate(&self, args: &RepayArgs) -> Result<()> {
        self.market.assert_started()?;
        require!(args.repay_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            self.owner_debt_account.amount,
            args.repay_amount,
            ErrorCode::InsufficientBalance
        );
        let repay_asset = self.market.asset_for_mint(self.debt_asset_mint.key())?;
        let debt_side = self.market.side(repay_asset);
        validate_debt_reserve_accounts(
            &self.market,
            debt_side,
            self.owner.key(),
            &self.debt_asset_mint,
            &self.reserve_vault,
            &self.owner_debt_account,
        )?;
        let interest_asset = validate_interest_accounts(&self.market, &self.debt_asset_mint, &self.interest_vault)?;
        require!(interest_asset == repay_asset, ErrorCode::InvalidVault);
        require_supported_asset_mint(&self.debt_asset_mint)?;
        self.borrow_position
            .assert_position(self.owner.key(), self.market.key())?;

        // Repayment must honor the referral binding stored for this debt side.
        let referral_partner = self.borrow_position.referral_partner(repay_asset);
        validate_referral_binding(
            None,
            referral_partner,
            self.borrow_position.referral_interest_share_bps(repay_asset),
            true,
            &self.futarchy_authority,
            self.referral_partner.as_deref(),
            self.referral_accrual.as_deref(),
            self.market.key(),
            &self.debt_asset_mint,
        )?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(RepayArgs);

    pub fn handle_repay(mut ctx: Context<'_, '_, '_, 'info, Self>, args: RepayArgs) -> Result<()> {
        let remaining_accounts = ctx.remaining_accounts;
        let (market_key, owner_key, debt_asset_mint_key, position_key, repay_gross, debt_receipt, referral_receipt) = {
            let accounts = &mut ctx.accounts;
            let market_key = accounts.market.key();
            let owner_key = accounts.owner.key();
            let debt_asset_mint_key = accounts.debt_asset_mint.key();
            let repay_asset = accounts.market.asset_for_mint(debt_asset_mint_key)?;
            let expected_referral_partner = accounts.borrow_position.referral_partner(repay_asset);
            let referral_interest_share_bps = accounts.borrow_position.referral_interest_share_bps(repay_asset);
            let reserve_balance_before = accounts.reserve_vault.amount;

            // Convert the user's gross limit into the exact reserve credit required.
            let debt_token_program = token_program_for_mint(
                &accounts.debt_asset_mint,
                &accounts.token_program,
                &accounts.token_2022_program,
            )?;
            let max_repay_credit = args
                .repay_amount
                .checked_sub(get_transfer_fee(
                    &accounts.debt_asset_mint.to_account_info(),
                    args.repay_amount,
                )?)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let repay_credit = accounts
                .market
                .fixed_repayment_for_max(&accounts.borrow_position, repay_asset, max_repay_credit)?
                .cash_repaid;
            let repay_gross = repay_credit
                .checked_add(get_transfer_inverse_fee(
                    &accounts.debt_asset_mint.to_account_info(),
                    repay_credit,
                )?)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require_gte!(args.repay_amount, repay_gross, ErrorCode::BrokenInvariant);

            // Transfer repayment and verify the reserve received the quoted credit.
            transfer_checked_with_remaining_accounts(
                accounts.owner.to_account_info(),
                accounts.owner_debt_account.to_account_info(),
                accounts.reserve_vault.to_account_info(),
                accounts.debt_asset_mint.to_account_info(),
                debt_token_program.clone(),
                repay_gross,
                accounts.debt_asset_mint.decimals,
                &[],
                remaining_accounts,
            )?;
            accounts.reserve_vault.reload()?;
            let measured_repay_credit = accounts
                .reserve_vault
                .amount
                .checked_sub(reserve_balance_before)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require_eq!(measured_repay_credit, repay_credit, ErrorCode::BrokenInvariant);

            let debt_receipt = accounts
                .market
                .repay(&mut accounts.borrow_position, repay_asset, repay_credit)?;
            require_eq!(debt_receipt.cash_repaid, repay_credit, ErrorCode::BrokenInvariant);

            // Move paid interest before splitting referral and protocol shares.
            let referral_receipt = if debt_receipt.interest_paid > 0 {
                let interest_vault_balance_before = accounts.interest_vault.amount;
                transfer_checked_with_remaining_accounts(
                    accounts.market.to_account_info(),
                    accounts.reserve_vault.to_account_info(),
                    accounts.interest_vault.to_account_info(),
                    accounts.debt_asset_mint.to_account_info(),
                    debt_token_program,
                    debt_receipt.interest_paid,
                    accounts.debt_asset_mint.decimals,
                    &[&generate_market_seeds!(accounts.market)[..]],
                    remaining_accounts,
                )?;
                accounts.reserve_vault.reload()?;
                accounts.interest_vault.reload()?;
                let interest_vault_credit =
                    token_account_credit(interest_vault_balance_before, &accounts.interest_vault)?;

                let revenue_share_interest_bps = accounts.futarchy_authority.revenue_share.interest_bps;
                let protocol_auction_split = accounts.futarchy_authority.protocol_auction_split;
                let referral_receipt = accrue_referral_interest(
                    expected_referral_partner,
                    referral_interest_share_bps,
                    &accounts.futarchy_authority,
                    accounts.referral_partner.as_deref(),
                    accounts.referral_accrual.as_deref_mut(),
                    market_key,
                    &accounts.debt_asset_mint,
                    debt_receipt.interest_paid,
                    interest_vault_credit,
                    revenue_share_interest_bps,
                )?;
                accounts.market.side_mut(repay_asset).record_interest_credit(
                    interest_vault_credit,
                    revenue_share_interest_bps,
                    protocol_auction_split,
                    referral_receipt.quote.referral_amount,
                )?;
                referral_receipt
            } else {
                accrue_referral_interest(
                    expected_referral_partner,
                    referral_interest_share_bps,
                    &accounts.futarchy_authority,
                    accounts.referral_partner.as_deref(),
                    accounts.referral_accrual.as_deref_mut(),
                    market_key,
                    &accounts.debt_asset_mint,
                    0,
                    0,
                    accounts.futarchy_authority.revenue_share.interest_bps,
                )?
            };

            // Principal and interest movements must leave reserve custody solvent.
            require_reserve_custody(accounts.reserve_vault.amount, accounts.market.side(repay_asset))?;

            (
                market_key,
                owner_key,
                debt_asset_mint_key,
                accounts.borrow_position.key(),
                repay_gross,
                debt_receipt,
                referral_receipt,
            )
        };

        // Finalize the curve transition and refresh risk after debt and cash move.
        let current_slot = Clock::get()?.slot;
        ctx.accounts.market.finalize_amm_transition(current_slot)?;
        ctx.accounts.market.refresh_risk()?;

        emit_cpi!(MarketDebtUpdated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_asset_mint_key,
            debt_delta: debt_receipt.debt_delta,
            cash_debit: repay_gross,
            cash_credit: debt_receipt.cash_repaid,
            interest_paid: debt_receipt.interest_paid,
            fixed_base_debt: debt_receipt.fixed_base_debt,
            fixed_quote_debt: debt_receipt.fixed_quote_debt,
            global_health_base_contribution_for_quote_debt: debt_receipt.global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: debt_receipt.global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: debt_receipt.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: debt_receipt.quote_liquidation_cf_bps,
            base_debt_health_bps: debt_receipt.base_debt_health_bps,
            quote_debt_health_bps: debt_receipt.quote_debt_health_bps,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });

        if let Some(event) = referral_interest_accrued_event_at_slot(
            &referral_receipt,
            market_key,
            position_key,
            owner_key,
            owner_key,
            debt_asset_mint_key,
            current_slot,
        )? {
            emit_cpi!(event);
        }

        let health = ctx.accounts.market.market_health()?;
        emit_cpi!(MarketHealthUpdated {
            market: market_key,
            global_health_base_contribution_for_quote_debt: health.global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: health.global_health_quote_contribution_for_base_debt,
            effective_base_debt_nad: health.effective_base_debt_nad,
            effective_quote_debt_nad: health.effective_quote_debt_nad,
            base_debt_health_bps: health.base_debt_health_bps,
            quote_debt_health_bps: health.quote_debt_health_bps,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}
