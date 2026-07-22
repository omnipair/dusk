use anchor_lang::prelude::*;

use crate::{constants::REFERRAL_PROFILE_SEED_PREFIX, events::ReferralRecipientUpdated, state::ReferralProfile};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetReferralRecipientArgs {
    pub recipient: Pubkey,
}

#[derive(Accounts)]
pub struct SetReferralRecipient<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [REFERRAL_PROFILE_SEED_PREFIX, authority.key().as_ref()],
        bump = referral_profile.bump
    )]
    pub referral_profile: Box<Account<'info, ReferralProfile>>,
}

impl<'info> SetReferralRecipient<'info> {
    pub fn handle_set(ctx: Context<Self>, args: SetReferralRecipientArgs) -> Result<()> {
        let authority = ctx.accounts.authority.key();
        ctx.accounts.referral_profile.set_recipient(authority, args.recipient)?;
        emit!(ReferralRecipientUpdated {
            referral_profile: ctx.accounts.referral_profile.key(),
            authority,
            recipient: args.recipient,
        });
        Ok(())
    }
}
