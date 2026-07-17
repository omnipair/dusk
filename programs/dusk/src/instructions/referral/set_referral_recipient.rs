use anchor_lang::prelude::*;

use crate::{
    constants::REFERRAL_PROFILE_SEED_PREFIX, events::ReferralRecipientUpdated,
    shared::account::get_size_with_discriminator, state::ReferralProfile,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetReferralRecipientArgs {
    pub recipient: Pubkey,
}

#[derive(Accounts)]
pub struct SetReferralRecipient<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init_if_needed,
        payer = authority,
        space = get_size_with_discriminator::<ReferralProfile>(),
        seeds = [REFERRAL_PROFILE_SEED_PREFIX, authority.key().as_ref()],
        bump
    )]
    pub referral_profile: Box<Account<'info, ReferralProfile>>,

    pub system_program: Program<'info, System>,
}

impl<'info> SetReferralRecipient<'info> {
    pub fn handle_set(ctx: Context<Self>, args: SetReferralRecipientArgs) -> Result<()> {
        let authority = ctx.accounts.authority.key();
        if ctx.accounts.referral_profile.authority == Pubkey::default() {
            ctx.accounts
                .referral_profile
                .initialize(authority, args.recipient, ctx.bumps.referral_profile)?;
        } else {
            ctx.accounts.referral_profile.set_recipient(authority, args.recipient)?;
        }
        emit!(ReferralRecipientUpdated {
            referral_profile: ctx.accounts.referral_profile.key(),
            authority,
            recipient: args.recipient,
        });
        Ok(())
    }
}
