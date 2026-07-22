use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::{MARKET_V2_SEED_PREFIX, REFERRAL_ACCRUAL_SEED_PREFIX, REFERRAL_PARTNER_SEED_PREFIX},
    errors::ErrorCode,
    events::{MarketEventMetadata, ReferralInterestClaimed},
    generate_market_seeds,
    instructions::common::{
        require_supported_asset_mint, token_account_credit, token_account_debit, token_program_for_mint,
        validate_interest_accounts,
    },
    shared::token::transfer_from_vault_to_user_with_remaining_accounts,
    state::{Market, ReferralAccrual, ReferralPartner},
};

#[derive(Accounts)]
pub struct ClaimReferralInterest<'info> {
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

    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [REFERRAL_PARTNER_SEED_PREFIX, authority.key().as_ref()],
        bump = referral_partner.bump,
        constraint = referral_partner.authority == authority.key() @ ErrorCode::InvalidReferralPartner
    )]
    pub referral_partner: Box<Account<'info, ReferralPartner>>,

    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [
            REFERRAL_ACCRUAL_SEED_PREFIX,
            referral_partner.key().as_ref(),
            market.key().as_ref(),
            asset_mint.key().as_ref(),
        ],
        bump = referral_accrual.bump,
        constraint = referral_accrual.referral_partner == referral_partner.key() @ ErrorCode::InvalidReferralAccrual,
        constraint = referral_accrual.market == market.key() @ ErrorCode::InvalidReferralAccrual,
        constraint = referral_accrual.asset_mint == asset_mint.key() @ ErrorCode::InvalidReferralAccrual
    )]
    pub referral_accrual: Box<Account<'info, ReferralAccrual>>,

    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub recipient_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> ClaimReferralInterest<'info> {
    pub fn validate(&self) -> Result<()> {
        require_supported_asset_mint(&self.asset_mint)?;
        validate_interest_accounts(&self.market, &self.asset_mint, &self.interest_vault)?;
        require_keys_eq!(
            self.recipient_token_account.owner,
            self.referral_partner.recipient,
            ErrorCode::InvalidRecipient
        );
        require_keys_eq!(
            self.recipient_token_account.mint,
            self.asset_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(
            *self.recipient_token_account.to_account_info().owner,
            *self.asset_mint.to_account_info().owner,
            ErrorCode::InvalidTokenProgram
        );
        require!(self.referral_accrual.amount > 0, ErrorCode::AmountZero);
        Ok(())
    }

    pub fn handle_claim(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let amount = ctx.accounts.referral_accrual.amount;
        let vault_balance_before = ctx.accounts.interest_vault.amount;
        let recipient_balance_before = ctx.accounts.recipient_token_account.amount;
        let token_program = token_program_for_mint(
            &ctx.accounts.asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_from_vault_to_user_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.interest_vault.to_account_info(),
            ctx.accounts.recipient_token_account.to_account_info(),
            ctx.accounts.asset_mint.to_account_info(),
            token_program,
            amount,
            ctx.accounts.asset_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.interest_vault.reload()?;
        ctx.accounts.recipient_token_account.reload()?;

        let vault_debit = token_account_debit(vault_balance_before, &ctx.accounts.interest_vault)?;
        let recipient_credit = token_account_credit(recipient_balance_before, &ctx.accounts.recipient_token_account)?;
        require_eq!(vault_debit, amount, ErrorCode::InvalidReferralAccrual);
        ctx.accounts.referral_accrual.claim(vault_debit)?;
        let asset = ctx.accounts.market.asset_for_mint(ctx.accounts.asset_mint.key())?;
        ctx.accounts
            .market
            .side_mut(asset)
            .settle_referral_interest_claim(vault_debit, ctx.accounts.interest_vault.amount)?;

        emit!(ReferralInterestClaimed {
            market: ctx.accounts.market.key(),
            referral_partner: ctx.accounts.referral_partner.key(),
            referral_accrual: ctx.accounts.referral_accrual.key(),
            authority: ctx.accounts.referral_partner.authority,
            recipient: ctx.accounts.referral_partner.recipient,
            asset_mint: ctx.accounts.asset_mint.key(),
            vault_debit,
            recipient_credit,
            remaining_accrual: ctx.accounts.referral_accrual.amount,
            metadata: MarketEventMetadata::new(ctx.accounts.authority.key(), ctx.accounts.market.key())?,
        });
        Ok(())
    }
}
