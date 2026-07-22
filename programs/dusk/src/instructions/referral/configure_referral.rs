use anchor_lang::prelude::*;

use crate::{
    constants::{FUTARCHY_AUTHORITY_SEED_PREFIX, REFERRAL_PROFILE_SEED_PREFIX},
    errors::ErrorCode,
    events::ReferralConfigured,
    shared::account::get_size_with_discriminator,
    state::{FutarchyAuthority, ReferralProfile},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ConfigureReferralArgs {
    pub referrer: Pubkey,
    pub interest_share_bps: u16,
    pub active: bool,
}

#[derive(Accounts)]
#[instruction(args: ConfigureReferralArgs)]
pub struct ConfigureReferral<'info> {
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
        space = get_size_with_discriminator::<ReferralProfile>(),
        seeds = [REFERRAL_PROFILE_SEED_PREFIX, args.referrer.as_ref()],
        bump
    )]
    pub referral_profile: Box<Account<'info, ReferralProfile>>,

    pub system_program: Program<'info, System>,
}

impl<'info> ConfigureReferral<'info> {
    pub fn handle_configure(ctx: Context<Self>, args: ConfigureReferralArgs) -> Result<()> {
        require_keys_neq!(args.referrer, Pubkey::default(), ErrorCode::InvalidReferralProfile);
        let profile = &mut ctx.accounts.referral_profile;
        if profile.authority == Pubkey::default() {
            profile.initialize(
                args.referrer,
                args.interest_share_bps,
                args.active,
                ctx.bumps.referral_profile,
            )?;
        } else {
            profile.configure(args.referrer, args.interest_share_bps, args.active)?;
        }

        emit!(ReferralConfigured {
            referral_profile: profile.key(),
            authority: profile.authority,
            recipient: profile.recipient,
            interest_share_bps: profile.interest_share_bps,
            active: profile.active,
            signer: ctx.accounts.authority_signer.key(),
        });
        Ok(())
    }
}
