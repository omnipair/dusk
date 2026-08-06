use anchor_lang::prelude::*;

use crate::{constants::REFERRAL_PARTNER_SEED_PREFIX, events::ReferralRecipientUpdated, state::ReferralPartner};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetReferralRecipientArgs {
    pub recipient: Pubkey,
}

#[event_cpi]
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
        let SetReferralRecipient {
            authority,
            referral_partner,
            ..
        } = ctx.accounts;
        let authority = authority.key();

        referral_partner.set_recipient(authority, args.recipient)?;
        emit_cpi!(ReferralRecipientUpdated {
            referral_partner: referral_partner.key(),
            authority,
            recipient: args.recipient,
        });
        Ok(())
    }
}
