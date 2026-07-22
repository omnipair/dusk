use anchor_lang::prelude::*;

use crate::{constants::REFERRAL_PARTNER_SEED_PREFIX, events::ReferralRecipientUpdated, state::ReferralPartner};

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
        seeds = [REFERRAL_PARTNER_SEED_PREFIX, authority.key().as_ref()],
        bump = referral_partner.bump
    )]
    pub referral_partner: Box<Account<'info, ReferralPartner>>,
}

impl<'info> SetReferralRecipient<'info> {
    pub fn handle_set(ctx: Context<Self>, args: SetReferralRecipientArgs) -> Result<()> {
        let authority = ctx.accounts.authority.key();
        ctx.accounts.referral_partner.set_recipient(authority, args.recipient)?;
        emit!(ReferralRecipientUpdated {
            referral_partner: ctx.accounts.referral_partner.key(),
            authority,
            recipient: args.recipient,
        });
        Ok(())
    }
}
