use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::{MAX_REFERRAL_INTEREST_SHARE_BPS, REFERRAL_ACCRUAL_SEED_PREFIX, REFERRAL_PARTNER_SEED_PREFIX},
    errors::ErrorCode,
    events::{MarketEventMetadata, ReferralInterestAccrued},
    state::{FutarchyAuthority, ReferralAccrual, ReferralInterestQuote, ReferralPartner},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValidatedReferralBinding {
    pub referrer: Option<Pubkey>,
    pub referral_partner: Option<Pubkey>,
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
        referrer: receipt.referrer.ok_or(ErrorCode::InvalidReferralPartner)?,
        referral_partner: receipt.referral_partner.ok_or(ErrorCode::InvalidReferralPartner)?,
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
    pub referral_partner: Option<Pubkey>,
    pub referral_accrual: Option<Pubkey>,
    pub quote: ReferralInterestQuote,
}

#[allow(clippy::too_many_arguments)]
pub fn validate_referral_binding<'info>(
    requested_referrer: Option<Pubkey>,
    current_referral_partner: Pubkey,
    current_interest_share_bps: u16,
    has_debt: bool,
    futarchy_authority: &FutarchyAuthority,
    referral_partner: Option<&Account<'info, ReferralPartner>>,
    referral_accrual: Option<&Account<'info, ReferralAccrual>>,
    market: Pubkey,
    asset_mint: &InterfaceAccount<'info, Mint>,
) -> Result<ValidatedReferralBinding> {
    futarchy_authority.validate_referral_interest_share_cap()?;
    if !has_debt {
        require_keys_eq!(current_referral_partner, Pubkey::default(), ErrorCode::BrokenInvariant);
        require_eq!(current_interest_share_bps, 0, ErrorCode::BrokenInvariant);
    }

    if has_debt && current_referral_partner == Pubkey::default() {
        require_eq!(current_interest_share_bps, 0, ErrorCode::BrokenInvariant);
        require!(requested_referrer.is_none(), ErrorCode::InvalidReferralPartner);
        require!(
            referral_partner.is_none() && referral_accrual.is_none(),
            ErrorCode::InvalidReferralPartner
        );
        return Ok(ValidatedReferralBinding::default());
    }

    if !has_debt && requested_referrer.is_none() {
        require!(
            referral_partner.is_none() && referral_accrual.is_none(),
            ErrorCode::InvalidReferralPartner
        );
        return Ok(ValidatedReferralBinding::default());
    }

    let partner = referral_partner.ok_or(ErrorCode::InvalidReferralPartner)?;
    let accrual = referral_accrual.ok_or(ErrorCode::InvalidReferralAccrual)?;
    if has_debt {
        require_keys_eq!(
            partner.key(),
            current_referral_partner,
            ErrorCode::InvalidReferralPartner
        );
        if let Some(referrer) = requested_referrer {
            require_keys_eq!(partner.authority, referrer, ErrorCode::InvalidReferralPartner);
        }
    } else {
        let referrer = requested_referrer.ok_or(ErrorCode::InvalidReferralPartner)?;
        require_keys_eq!(partner.authority, referrer, ErrorCode::InvalidReferralPartner);
    }
    validate_partner_and_accrual(partner, accrual, market, asset_mint.key())?;
    let interest_share_bps = if has_debt {
        require_gte!(
            MAX_REFERRAL_INTEREST_SHARE_BPS,
            current_interest_share_bps,
            ErrorCode::InvalidReferralInterestShareBps
        );
        current_interest_share_bps
    } else {
        partner.binding_interest_share_bps(futarchy_authority.max_referral_interest_share_bps)?
    };

    Ok(ValidatedReferralBinding {
        referrer: Some(partner.authority),
        referral_partner: Some(partner.key()),
        referral_accrual: Some(accrual.key()),
        interest_share_bps,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn accrue_referral_interest<'info>(
    expected_referral_partner: Pubkey,
    interest_share_bps: u16,
    futarchy_authority: &FutarchyAuthority,
    referral_partner: Option<&Account<'info, ReferralPartner>>,
    referral_accrual: Option<&mut Account<'info, ReferralAccrual>>,
    market: Pubkey,
    asset_mint: &InterfaceAccount<'info, Mint>,
    interest_paid: u64,
    interest_vault_credit: u64,
    protocol_interest_bps: u16,
) -> Result<ReferralInterestAccrualReceipt> {
    futarchy_authority.validate_referral_interest_share_cap()?;
    if expected_referral_partner == Pubkey::default() {
        require_eq!(interest_share_bps, 0, ErrorCode::BrokenInvariant);
        require!(
            referral_partner.is_none() && referral_accrual.is_none(),
            ErrorCode::InvalidReferralPartner
        );
        return Ok(ReferralInterestAccrualReceipt {
            quote: ReferralInterestQuote::new(interest_paid, interest_vault_credit, protocol_interest_bps, None)?,
            ..ReferralInterestAccrualReceipt::default()
        });
    }

    let partner = referral_partner.ok_or(ErrorCode::InvalidReferralPartner)?;
    let accrual = referral_accrual.ok_or(ErrorCode::InvalidReferralAccrual)?;
    require_keys_eq!(
        partner.key(),
        expected_referral_partner,
        ErrorCode::InvalidReferralPartner
    );
    validate_partner_and_accrual(partner, accrual, market, asset_mint.key())?;

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
        referrer: Some(partner.authority),
        referral_partner: Some(partner.key()),
        referral_accrual: Some(accrual.key()),
        quote,
    })
}

fn validate_partner_and_accrual(
    partner: &Account<ReferralPartner>,
    accrual: &Account<ReferralAccrual>,
    market: Pubkey,
    asset_mint: Pubkey,
) -> Result<()> {
    let (expected_partner, partner_bump) =
        Pubkey::find_program_address(&[REFERRAL_PARTNER_SEED_PREFIX, partner.authority.as_ref()], &crate::ID);
    require_keys_eq!(partner.key(), expected_partner, ErrorCode::InvalidReferralPartner);
    require_eq!(partner.bump, partner_bump, ErrorCode::InvalidReferralPartner);

    let (expected_accrual, accrual_bump) = Pubkey::find_program_address(
        &[
            REFERRAL_ACCRUAL_SEED_PREFIX,
            partner.key().as_ref(),
            market.as_ref(),
            asset_mint.as_ref(),
        ],
        &crate::ID,
    );
    require_keys_eq!(accrual.key(), expected_accrual, ErrorCode::InvalidReferralAccrual);
    require_eq!(accrual.bump, accrual_bump, ErrorCode::InvalidReferralAccrual);
    require_keys_eq!(
        accrual.referral_partner,
        partner.key(),
        ErrorCode::InvalidReferralAccrual
    );
    require_keys_eq!(accrual.market, market, ErrorCode::InvalidReferralAccrual);
    require_keys_eq!(accrual.asset_mint, asset_mint, ErrorCode::InvalidReferralAccrual);
    Ok(())
}
