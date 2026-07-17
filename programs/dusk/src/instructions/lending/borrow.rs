use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketDebtUpdated, MarketEventMetadata, MarketHealthUpdated, ReferralOriginationFeePaid},
    generate_market_seeds,
    shared::token::transfer_from_vault_to_user_with_remaining_accounts,
    state::{BorrowPosition, FutarchyAuthority, Market, ReferralAction, ReferralProfile},
};

use crate::instructions::common::{require_supported_asset_mint, token_account_credit, token_program_for_mint};
use crate::instructions::referral::common::{pay_referral_fee, validate_referral};

use super::common::validate_borrow_accounts;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BorrowArgs {
    pub borrow_amount: u64,
    pub min_debt_amount_out: u64,
    pub min_liquidation_cf_bps: u16,
    pub referrer: Option<Pubkey>,
    pub max_acceptable_referral_fee_bps: Option<u16>,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: BorrowArgs)]
pub struct Borrow<'info> {
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

    pub collateral_asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

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
    pub referral_vault: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> Borrow<'info> {
    pub fn validate(&self, args: &BorrowArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(args.borrow_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            args.borrow_amount,
            args.min_debt_amount_out,
            ErrorCode::SlippageExceeded
        );
        validate_borrow_accounts(
            &self.market,
            self.owner.key(),
            &self.debt_asset_mint,
            &self.collateral_asset_mint,
            &self.reserve_vault,
            &self.owner_debt_account,
        )?;
        require_supported_asset_mint(&self.debt_asset_mint)?;
        self.borrow_position
            .assert_position(self.owner.key(), self.market.key())?;
        validate_referral(
            args.borrow_amount,
            args.referrer,
            args.max_acceptable_referral_fee_bps,
            &self.futarchy_authority,
            self.referral_profile.as_deref(),
            self.referral_vault.as_deref(),
            &self.debt_asset_mint,
        )?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(BorrowArgs);

    pub fn handle_borrow(mut ctx: Context<'_, '_, '_, 'info, Self>, args: BorrowArgs) -> Result<()> {
        let (market_key, owner_key, debt_asset_mint_key, position_key, debt_receipt, referral, referral_receipt) = {
            let accounts = &mut ctx.accounts;
            let market_key = accounts.market.key();
            let owner_key = accounts.owner.key();
            let debt_asset_mint_key = accounts.debt_asset_mint.key();
            let borrow_asset = accounts.market.asset_for_mint(debt_asset_mint_key)?;
            let referral = validate_referral(
                args.borrow_amount,
                args.referrer,
                args.max_acceptable_referral_fee_bps,
                &accounts.futarchy_authority,
                accounts.referral_profile.as_deref(),
                accounts.referral_vault.as_deref(),
                &accounts.debt_asset_mint,
            )?;

            let debt_receipt = accounts.market.borrow(
                &mut accounts.borrow_position,
                borrow_asset,
                referral.quote.gross_debt,
                args.min_liquidation_cf_bps,
            )?;

            let debt_token_program = token_program_for_mint(
                &accounts.debt_asset_mint,
                &accounts.token_program,
                &accounts.token_2022_program,
            )?;
            let owner_debt_balance_before = accounts.owner_debt_account.amount;

            transfer_from_vault_to_user_with_remaining_accounts(
                accounts.market.to_account_info(),
                accounts.reserve_vault.to_account_info(),
                accounts.owner_debt_account.to_account_info(),
                accounts.debt_asset_mint.to_account_info(),
                debt_token_program,
                args.borrow_amount,
                accounts.debt_asset_mint.decimals,
                &[&generate_market_seeds!(accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            accounts.owner_debt_account.reload()?;
            let debt_credit = token_account_credit(owner_debt_balance_before, &accounts.owner_debt_account)?;
            require_gte!(debt_credit, args.min_debt_amount_out, ErrorCode::SlippageExceeded);

            let referral_receipt = pay_referral_fee(
                &accounts.market,
                &mut accounts.reserve_vault,
                accounts.referral_vault.as_deref_mut(),
                &accounts.debt_asset_mint,
                &accounts.token_program,
                &accounts.token_2022_program,
                referral,
                ctx.remaining_accounts,
            )?;

            (
                market_key,
                owner_key,
                debt_asset_mint_key,
                accounts.borrow_position.key(),
                debt_receipt,
                referral,
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

        if let (Some(referrer), Some(referral_profile)) = (referral.referrer, referral.referral_profile) {
            emit_cpi!(ReferralOriginationFeePaid {
                market: market_key,
                position: position_key,
                owner: owner_key,
                referrer,
                referral_profile,
                asset_mint: debt_asset_mint_key,
                action: ReferralAction::Borrow,
                requested_principal: referral_receipt.requested_principal,
                configured_fee_bps: referral_receipt.configured_fee_bps,
                fee_debit: referral_receipt.fee_debit,
                vault_credit: referral_receipt.vault_credit,
                gross_debt: referral_receipt.gross_debt,
                metadata: MarketEventMetadata::new(owner_key, market_key)?,
            });
        }

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
