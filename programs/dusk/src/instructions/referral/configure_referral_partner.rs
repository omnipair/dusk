use anchor_lang::prelude::*;

use crate::{
    constants::{FUTARCHY_AUTHORITY_SEED_PREFIX, REFERRAL_PARTNER_SEED_PREFIX},
    errors::ErrorCode,
    events::ReferralPartnerConfigured,
    shared::account::get_size_with_discriminator,
    state::{FutarchyAuthority, ReferralPartner},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ConfigureReferralPartnerArgs {
    pub referrer: Pubkey,
    pub interest_share_bps: u16,
    pub active: bool,
}

#[derive(Accounts)]
#[instruction(args: ConfigureReferralPartnerArgs)]
pub struct ConfigureReferralPartner<'info> {
    #[account(
        mut,
        address = futarchy_authority.authority @ ErrorCode::InvalidFutarchyAuthority
    )]
    pub authority_signer: Signer<'info>,

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(
        init_if_needed,
        payer = authority_signer,
        space = get_size_with_discriminator::<ReferralPartner>(),
        seeds = [REFERRAL_PARTNER_SEED_PREFIX, args.referrer.as_ref()],
        bump
    )]
    pub referral_partner: Box<Account<'info, ReferralPartner>>,

    pub system_program: Program<'info, System>,
}

impl<'info> ConfigureReferralPartner<'info> {
    pub fn handle_configure(ctx: Context<Self>, args: ConfigureReferralPartnerArgs) -> Result<()> {
        require_keys_neq!(args.referrer, Pubkey::default(), ErrorCode::InvalidReferralPartner);
        let partner = &mut ctx.accounts.referral_partner;
        if partner.authority == Pubkey::default() {
            partner.initialize(
                args.referrer,
                args.interest_share_bps,
                args.active,
                ctx.bumps.referral_partner,
            )?;
        } else {
            partner.configure(args.referrer, args.interest_share_bps, args.active)?;
        }

        emit!(ReferralPartnerConfigured {
            referral_partner: partner.key(),
            authority: partner.authority,
            recipient: partner.recipient,
            interest_share_bps: partner.interest_share_bps,
            active: partner.active,
            signer: ctx.accounts.authority_signer.key(),
        });
        Ok(())
    }
}
