use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::{MAX_REFERRAL_INTEREST_SHARE_BPS, REFERRAL_ACCRUAL_SEED_PREFIX, REFERRAL_PROFILE_SEED_PREFIX},
    errors::ErrorCode,
    events::{MarketEventMetadata, ReferralInterestAccrued},
    state::{FutarchyAuthority, ReferralAccrual, ReferralInterestQuote, ReferralProfile},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValidatedReferralBinding {
    pub referrer: Option<Pubkey>,
    pub referral_profile: Option<Pubkey>,
    pub referral_accrual: Option<Pubkey>,
    pub interest_share_bps: u16,
}

pub fn emit_referral_interest_accrued(
    receipt: &ReferralInterestAccrualReceipt,
    market: Pubkey,
    position: Pubkey,
    owner: Pubkey,
    signer: Pubkey,
    asset_mint: Pubkey,
) -> Result<()> {
    if receipt.quote.referral_amount == 0 {
        return Ok(());
    }
    emit!(ReferralInterestAccrued {
        market,
        position,
        owner,
        referrer: receipt.referrer.ok_or(ErrorCode::InvalidReferralProfile)?,
        referral_profile: receipt.referral_profile.ok_or(ErrorCode::InvalidReferralProfile)?,
        referral_accrual: receipt.referral_accrual.ok_or(ErrorCode::InvalidReferralAccrual)?,
        asset_mint,
        interest_paid: receipt.quote.interest_paid,
        interest_vault_credit: receipt.quote.interest_vault_credit,
        protocol_interest_revenue: receipt.quote.protocol_interest_revenue,
        interest_share_bps: receipt.quote.interest_share_bps,
        accrued_amount: receipt.quote.referral_amount,
        metadata: MarketEventMetadata::new(signer, market)?,
    });
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferralInterestAccrualReceipt {
    pub referrer: Option<Pubkey>,
    pub referral_profile: Option<Pubkey>,
    pub referral_accrual: Option<Pubkey>,
    pub quote: ReferralInterestQuote,
}

#[allow(clippy::too_many_arguments)]
pub fn validate_referral_binding<'info>(
    requested_referrer: Option<Pubkey>,
    current_referral_profile: Pubkey,
    current_interest_share_bps: u16,
    has_debt: bool,
    futarchy_authority: &FutarchyAuthority,
    referral_profile: Option<&Account<'info, ReferralProfile>>,
    referral_accrual: Option<&Account<'info, ReferralAccrual>>,
    market: Pubkey,
    asset_mint: &InterfaceAccount<'info, Mint>,
) -> Result<ValidatedReferralBinding> {
    futarchy_authority.validate_referral_interest_share_cap()?;
    if !has_debt {
        require_keys_eq!(current_referral_profile, Pubkey::default(), ErrorCode::BrokenInvariant);
        require_eq!(current_interest_share_bps, 0, ErrorCode::BrokenInvariant);
    }

    if has_debt && current_referral_profile == Pubkey::default() {
        require_eq!(current_interest_share_bps, 0, ErrorCode::BrokenInvariant);
        require!(requested_referrer.is_none(), ErrorCode::InvalidReferralProfile);
        require!(
            referral_profile.is_none() && referral_accrual.is_none(),
            ErrorCode::InvalidReferralProfile
        );
        return Ok(ValidatedReferralBinding::default());
    }

    if !has_debt && requested_referrer.is_none() {
        require!(
            referral_profile.is_none() && referral_accrual.is_none(),
            ErrorCode::InvalidReferralProfile
        );
        return Ok(ValidatedReferralBinding::default());
    }

    let profile = referral_profile.ok_or(ErrorCode::InvalidReferralProfile)?;
    let accrual = referral_accrual.ok_or(ErrorCode::InvalidReferralAccrual)?;
    if has_debt {
        require_keys_eq!(
            profile.key(),
            current_referral_profile,
            ErrorCode::InvalidReferralProfile
        );
        if let Some(referrer) = requested_referrer {
            require_keys_eq!(profile.authority, referrer, ErrorCode::InvalidReferralProfile);
        }
    } else {
        let referrer = requested_referrer.ok_or(ErrorCode::InvalidReferralProfile)?;
        require_keys_eq!(profile.authority, referrer, ErrorCode::InvalidReferralProfile);
    }
    validate_profile_and_accrual(profile, accrual, market, asset_mint.key())?;
    let interest_share_bps = if has_debt {
        require_gte!(
            MAX_REFERRAL_INTEREST_SHARE_BPS,
            current_interest_share_bps,
            ErrorCode::InvalidReferralInterestShareBps
        );
        current_interest_share_bps
    } else {
        profile.binding_interest_share_bps(futarchy_authority.max_referral_interest_share_bps)?
    };

    Ok(ValidatedReferralBinding {
        referrer: Some(profile.authority),
        referral_profile: Some(profile.key()),
        referral_accrual: Some(accrual.key()),
        interest_share_bps,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn accrue_referral_interest<'info>(
    expected_referral_profile: Pubkey,
    interest_share_bps: u16,
    futarchy_authority: &FutarchyAuthority,
    referral_profile: Option<&Account<'info, ReferralProfile>>,
    referral_accrual: Option<&mut Account<'info, ReferralAccrual>>,
    market: Pubkey,
    asset_mint: &InterfaceAccount<'info, Mint>,
    interest_paid: u64,
    interest_vault_credit: u64,
    protocol_interest_bps: u16,
) -> Result<ReferralInterestAccrualReceipt> {
    futarchy_authority.validate_referral_interest_share_cap()?;
    if expected_referral_profile == Pubkey::default() {
        require_eq!(interest_share_bps, 0, ErrorCode::BrokenInvariant);
        require!(
            referral_profile.is_none() && referral_accrual.is_none(),
            ErrorCode::InvalidReferralProfile
        );
        return Ok(ReferralInterestAccrualReceipt {
            quote: ReferralInterestQuote::new(interest_paid, interest_vault_credit, protocol_interest_bps, None)?,
            ..ReferralInterestAccrualReceipt::default()
        });
    }

    let profile = referral_profile.ok_or(ErrorCode::InvalidReferralProfile)?;
    let accrual = referral_accrual.ok_or(ErrorCode::InvalidReferralAccrual)?;
    require_keys_eq!(
        profile.key(),
        expected_referral_profile,
        ErrorCode::InvalidReferralProfile
    );
    validate_profile_and_accrual(profile, accrual, market, asset_mint.key())?;

    let quote = ReferralInterestQuote::new(
        interest_paid,
        interest_vault_credit,
        protocol_interest_bps,
        Some(interest_share_bps),
    )?;
    if quote.referral_amount > 0 {
        accrual.accrue(quote.referral_amount)?;
    }
    Ok(ReferralInterestAccrualReceipt {
        referrer: Some(profile.authority),
        referral_profile: Some(profile.key()),
        referral_accrual: Some(accrual.key()),
        quote,
    })
}

fn validate_profile_and_accrual(
    profile: &Account<ReferralProfile>,
    accrual: &Account<ReferralAccrual>,
    market: Pubkey,
    asset_mint: Pubkey,
) -> Result<()> {
    let (expected_profile, profile_bump) =
        Pubkey::find_program_address(&[REFERRAL_PROFILE_SEED_PREFIX, profile.authority.as_ref()], &crate::ID);
    require_keys_eq!(profile.key(), expected_profile, ErrorCode::InvalidReferralProfile);
    require_eq!(profile.bump, profile_bump, ErrorCode::InvalidReferralProfile);

    let (expected_accrual, accrual_bump) = Pubkey::find_program_address(
        &[
            REFERRAL_ACCRUAL_SEED_PREFIX,
            profile.key().as_ref(),
            market.as_ref(),
            asset_mint.as_ref(),
        ],
        &crate::ID,
    );
    require_keys_eq!(accrual.key(), expected_accrual, ErrorCode::InvalidReferralAccrual);
    require_eq!(accrual.bump, accrual_bump, ErrorCode::InvalidReferralAccrual);
    require_keys_eq!(
        accrual.referral_profile,
        profile.key(),
        ErrorCode::InvalidReferralAccrual
    );
    require_keys_eq!(accrual.market, market, ErrorCode::InvalidReferralAccrual);
    require_keys_eq!(accrual.asset_mint, asset_mint, ErrorCode::InvalidReferralAccrual);
    Ok(())
}
