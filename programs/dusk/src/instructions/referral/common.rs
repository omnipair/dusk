use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::get_associated_token_address_with_program_id,
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::{MARKET_V2_SEED_PREFIX, REFERRAL_PROFILE_SEED_PREFIX},
    errors::ErrorCode,
    generate_market_seeds,
    instructions::common::{token_account_credit, token_program_for_mint},
    shared::token::transfer_from_vault_to_vault_with_remaining_accounts,
    state::{FutarchyAuthority, Market, ReferralFeeQuote, ReferralFeeReceipt, ReferralProfile},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValidatedReferral {
    pub referrer: Option<Pubkey>,
    pub referral_profile: Option<Pubkey>,
    pub quote: ReferralFeeQuote,
}

pub fn validate_referral<'info>(
    requested_principal: u64,
    referrer: Option<Pubkey>,
    max_acceptable_fee_bps: Option<u16>,
    futarchy_authority: &FutarchyAuthority,
    referral_profile: Option<&Account<'info, ReferralProfile>>,
    referral_vault: Option<&InterfaceAccount<'info, TokenAccount>>,
    asset_mint: &InterfaceAccount<'info, Mint>,
) -> Result<ValidatedReferral> {
    futarchy_authority.validate_referral_origination_fee()?;
    require!(
        referrer.is_some() == max_acceptable_fee_bps.is_some(),
        ErrorCode::InvalidArgument
    );
    let max_acceptable_fee_bps = max_acceptable_fee_bps.unwrap_or_default();
    let quote = ReferralFeeQuote::new(
        requested_principal,
        futarchy_authority.referral_origination_fee_bps,
        max_acceptable_fee_bps,
        referrer.is_some(),
    )?;

    let Some(referrer) = referrer else {
        require!(
            referral_profile.is_none() && referral_vault.is_none(),
            ErrorCode::InvalidReferralProfile
        );
        return Ok(ValidatedReferral {
            quote,
            ..ValidatedReferral::default()
        });
    };

    let referral_profile = referral_profile.ok_or(ErrorCode::InvalidReferralProfile)?;
    let referral_vault = referral_vault.ok_or(ErrorCode::InvalidReferralVault)?;
    require_keys_eq!(referral_profile.authority, referrer, ErrorCode::InvalidReferralProfile);
    let (expected_profile, expected_bump) =
        Pubkey::find_program_address(&[REFERRAL_PROFILE_SEED_PREFIX, referrer.as_ref()], &crate::ID);
    require_keys_eq!(
        referral_profile.key(),
        expected_profile,
        ErrorCode::InvalidReferralProfile
    );
    require_eq!(referral_profile.bump, expected_bump, ErrorCode::InvalidReferralProfile);

    let mint_program = *asset_mint.to_account_info().owner;
    let expected_vault =
        get_associated_token_address_with_program_id(&referral_profile.key(), &asset_mint.key(), &mint_program);
    require_keys_eq!(referral_vault.key(), expected_vault, ErrorCode::InvalidReferralVault);
    require_keys_eq!(
        referral_vault.owner,
        referral_profile.key(),
        ErrorCode::InvalidReferralVault
    );
    require_keys_eq!(referral_vault.mint, asset_mint.key(), ErrorCode::InvalidReferralVault);
    require_keys_eq!(
        *referral_vault.to_account_info().owner,
        mint_program,
        ErrorCode::InvalidReferralVault
    );

    Ok(ValidatedReferral {
        referrer: Some(referrer),
        referral_profile: Some(referral_profile.key()),
        quote,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn pay_referral_fee<'info>(
    market: &Account<'info, Market>,
    reserve_vault: &mut InterfaceAccount<'info, TokenAccount>,
    referral_vault: Option<&mut InterfaceAccount<'info, TokenAccount>>,
    asset_mint: &InterfaceAccount<'info, Mint>,
    token_program: &Program<'info, Token>,
    token_2022_program: &Program<'info, Token2022>,
    referral: ValidatedReferral,
    additional_accounts: &[AccountInfo<'info>],
) -> Result<ReferralFeeReceipt> {
    if referral.referrer.is_none() || referral.quote.fee_debit == 0 {
        return ReferralFeeReceipt::new(referral.quote, 0);
    }
    let referral_vault = referral_vault.ok_or(ErrorCode::InvalidReferralVault)?;
    let vault_balance_before = referral_vault.amount;
    let asset_token_program = token_program_for_mint(asset_mint, token_program, token_2022_program)?;
    transfer_from_vault_to_vault_with_remaining_accounts(
        market.to_account_info(),
        reserve_vault.to_account_info(),
        referral_vault.to_account_info(),
        asset_mint.to_account_info(),
        asset_token_program,
        referral.quote.fee_debit,
        asset_mint.decimals,
        &[&generate_market_seeds!(market)[..]],
        additional_accounts,
    )?;
    reserve_vault.reload()?;
    referral_vault.reload()?;
    ReferralFeeReceipt::new(
        referral.quote,
        token_account_credit(vault_balance_before, referral_vault)?,
    )
}
