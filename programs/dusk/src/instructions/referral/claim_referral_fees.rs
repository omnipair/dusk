use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::get_associated_token_address_with_program_id,
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::REFERRAL_PROFILE_SEED_PREFIX,
    errors::ErrorCode,
    events::ReferralFeesClaimed,
    instructions::common::{
        require_supported_asset_mint, token_account_credit, token_account_debit, token_program_for_mint,
    },
    shared::token::transfer_from_vault_to_user_with_remaining_accounts,
    state::ReferralProfile,
};

#[derive(Accounts)]
pub struct ClaimReferralFees<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [REFERRAL_PROFILE_SEED_PREFIX, authority.key().as_ref()],
        bump = referral_profile.bump,
        constraint = referral_profile.authority == authority.key() @ ErrorCode::InvalidReferralProfile
    )]
    pub referral_profile: Box<Account<'info, ReferralProfile>>,

    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub referral_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub recipient_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> ClaimReferralFees<'info> {
    pub fn validate(&self) -> Result<()> {
        require_supported_asset_mint(&self.asset_mint)?;
        let mint_program = *self.asset_mint.to_account_info().owner;
        let expected_vault = get_associated_token_address_with_program_id(
            &self.referral_profile.key(),
            &self.asset_mint.key(),
            &mint_program,
        );
        require_keys_eq!(
            self.referral_vault.key(),
            expected_vault,
            ErrorCode::InvalidReferralVault
        );
        require_keys_eq!(
            self.referral_vault.owner,
            self.referral_profile.key(),
            ErrorCode::InvalidReferralVault
        );
        require_keys_eq!(
            self.referral_vault.mint,
            self.asset_mint.key(),
            ErrorCode::InvalidReferralVault
        );
        require_keys_eq!(
            *self.referral_vault.to_account_info().owner,
            mint_program,
            ErrorCode::InvalidReferralVault
        );
        require_keys_eq!(
            self.recipient_token_account.owner,
            self.referral_profile.recipient,
            ErrorCode::InvalidRecipient
        );
        require_keys_eq!(
            self.recipient_token_account.mint,
            self.asset_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(
            *self.recipient_token_account.to_account_info().owner,
            mint_program,
            ErrorCode::InvalidTokenProgram
        );
        require!(self.referral_vault.amount > 0, ErrorCode::AmountZero);
        Ok(())
    }

    pub fn handle_claim(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let amount = ctx.accounts.referral_vault.amount;
        let vault_balance_before = amount;
        let recipient_balance_before = ctx.accounts.recipient_token_account.amount;
        let authority_key = ctx.accounts.referral_profile.authority;
        let bump = [ctx.accounts.referral_profile.bump];
        let signer_seeds = [REFERRAL_PROFILE_SEED_PREFIX, authority_key.as_ref(), bump.as_ref()];
        let token_program = token_program_for_mint(
            &ctx.accounts.asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_from_vault_to_user_with_remaining_accounts(
            ctx.accounts.referral_profile.to_account_info(),
            ctx.accounts.referral_vault.to_account_info(),
            ctx.accounts.recipient_token_account.to_account_info(),
            ctx.accounts.asset_mint.to_account_info(),
            token_program,
            amount,
            ctx.accounts.asset_mint.decimals,
            &[&signer_seeds],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.referral_vault.reload()?;
        ctx.accounts.recipient_token_account.reload()?;
        let vault_debit = token_account_debit(vault_balance_before, &ctx.accounts.referral_vault)?;
        let recipient_credit = token_account_credit(recipient_balance_before, &ctx.accounts.recipient_token_account)?;
        require_eq!(ctx.accounts.referral_vault.amount, 0, ErrorCode::InvalidReferralVault);
        emit!(ReferralFeesClaimed {
            referral_profile: ctx.accounts.referral_profile.key(),
            authority: authority_key,
            recipient: ctx.accounts.referral_profile.recipient,
            asset_mint: ctx.accounts.asset_mint.key(),
            vault_debit,
            recipient_credit,
        });
        Ok(())
    }
}
