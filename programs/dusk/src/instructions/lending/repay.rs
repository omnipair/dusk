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
    shared::token::{
        transfer_from_user_to_vault_with_remaining_accounts, transfer_from_vault_to_vault_with_remaining_accounts,
    },
    state::{BorrowPosition, FutarchyAuthority, Market, ReferralAccrual, ReferralProfile},
};

use crate::instructions::common::{
    require_supported_asset_mint, token_account_credit, token_program_for_mint, validate_interest_accounts,
};

use super::common::validate_repay_accounts;
use crate::instructions::referral::common::{
    accrue_referral_interest, emit_referral_interest_accrued, validate_referral_binding,
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

    pub referral_profile: Option<Box<Account<'info, ReferralProfile>>>,

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
        let repay_asset = validate_repay_accounts(
            &self.market,
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
        let referral_profile = self.borrow_position.referral_profile(repay_asset);
        validate_referral_binding(
            None,
            referral_profile,
            self.borrow_position.referral_interest_share_bps(repay_asset),
            true,
            &self.futarchy_authority,
            self.referral_profile.as_deref(),
            self.referral_accrual.as_deref(),
            self.market.key(),
            &self.debt_asset_mint,
        )?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(RepayArgs);

    pub fn handle_repay(mut ctx: Context<'_, '_, '_, 'info, Self>, args: RepayArgs) -> Result<()> {
        let remaining_accounts = ctx.remaining_accounts;
        let (market_key, owner_key, debt_asset_mint_key, position_key, debt_receipt, referral_receipt) = {
            let accounts = &mut ctx.accounts;
            let market_key = accounts.market.key();
            let owner_key = accounts.owner.key();
            let debt_asset_mint_key = accounts.debt_asset_mint.key();
            let repay_asset = accounts.market.asset_for_mint(debt_asset_mint_key)?;
            let expected_referral_profile = accounts.borrow_position.referral_profile(repay_asset);
            let referral_interest_share_bps = accounts.borrow_position.referral_interest_share_bps(repay_asset);
            let reserve_balance_before = accounts.reserve_vault.amount;

            let debt_token_program = token_program_for_mint(
                &accounts.debt_asset_mint,
                &accounts.token_program,
                &accounts.token_2022_program,
            )?;
            transfer_from_user_to_vault_with_remaining_accounts(
                accounts.owner.to_account_info(),
                accounts.owner_debt_account.to_account_info(),
                accounts.reserve_vault.to_account_info(),
                accounts.debt_asset_mint.to_account_info(),
                debt_token_program.clone(),
                args.repay_amount,
                accounts.debt_asset_mint.decimals,
                remaining_accounts,
            )?;
            accounts.reserve_vault.reload()?;
            let repay_credit = accounts
                .reserve_vault
                .amount
                .checked_sub(reserve_balance_before)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require!(repay_credit > 0, ErrorCode::AmountZero);

            let debt_receipt = accounts
                .market
                .repay(&mut accounts.borrow_position, repay_asset, repay_credit)?;

            let referral_receipt = if debt_receipt.interest_paid > 0 {
                let interest_vault_balance_before = accounts.interest_vault.amount;
                transfer_from_vault_to_vault_with_remaining_accounts(
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
                accounts.interest_vault.reload()?;
                let interest_vault_credit =
                    token_account_credit(interest_vault_balance_before, &accounts.interest_vault)?;

                let manager_fee_bps = accounts.market.config.manager_fee_bps;
                let revenue_share_interest_bps = accounts.futarchy_authority.revenue_share.interest_bps;
                let protocol_auction_split = accounts.futarchy_authority.protocol_auction_split;
                let referral_receipt = accrue_referral_interest(
                    expected_referral_profile,
                    referral_interest_share_bps,
                    &accounts.futarchy_authority,
                    accounts.referral_profile.as_deref(),
                    accounts.referral_accrual.as_deref_mut(),
                    market_key,
                    &accounts.debt_asset_mint,
                    debt_receipt.interest_paid,
                    interest_vault_credit,
                    revenue_share_interest_bps,
                )?;
                accounts.market.side_mut(repay_asset).record_interest_credit(
                    interest_vault_credit,
                    manager_fee_bps,
                    revenue_share_interest_bps,
                    protocol_auction_split,
                    referral_receipt.quote.referral_amount,
                )?;
                referral_receipt
            } else {
                accrue_referral_interest(
                    expected_referral_profile,
                    referral_interest_share_bps,
                    &accounts.futarchy_authority,
                    accounts.referral_profile.as_deref(),
                    accounts.referral_accrual.as_deref_mut(),
                    market_key,
                    &accounts.debt_asset_mint,
                    0,
                    0,
                    accounts.futarchy_authority.revenue_share.interest_bps,
                )?
            };

            (
                market_key,
                owner_key,
                debt_asset_mint_key,
                accounts.borrow_position.key(),
                debt_receipt,
                referral_receipt,
            )
        };

        emit_cpi!(MarketDebtUpdated {
            market: market_key,
            owner: owner_key,
            debt_asset_mint: debt_asset_mint_key,
            debt_delta: debt_receipt.debt_delta,
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

        emit_referral_interest_accrued(
            &referral_receipt,
            market_key,
            position_key,
            owner_key,
            owner_key,
            debt_asset_mint_key,
        )?;

        let health = ctx.accounts.market.market_health()?;
        emit!(MarketHealthUpdated {
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
