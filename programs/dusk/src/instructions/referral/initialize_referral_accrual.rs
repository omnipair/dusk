use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::{MARKET_V2_SEED_PREFIX, REFERRAL_ACCRUAL_SEED_PREFIX, REFERRAL_PARTNER_SEED_PREFIX},
    errors::ErrorCode,
    shared::account::get_size_with_discriminator,
    state::{Market, ReferralAccrual, ReferralPartner},
};

#[derive(Accounts)]
pub struct InitializeReferralAccrual<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        seeds = [REFERRAL_PARTNER_SEED_PREFIX, referral_partner.authority.as_ref()],
        bump = referral_partner.bump
    )]
    pub referral_partner: Box<Account<'info, ReferralPartner>>,

    #[account(
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        init_if_needed,
        payer = payer,
        space = get_size_with_discriminator::<ReferralAccrual>(),
        seeds = [
            REFERRAL_ACCRUAL_SEED_PREFIX,
            referral_partner.key().as_ref(),
            market.key().as_ref(),
            asset_mint.key().as_ref(),
        ],
        bump
    )]
    pub referral_accrual: Box<Account<'info, ReferralAccrual>>,

    pub system_program: Program<'info, System>,
}

impl<'info> InitializeReferralAccrual<'info> {
    pub fn handle_initialize(ctx: Context<Self>) -> Result<()> {
        ctx.accounts.market.asset_for_mint(ctx.accounts.asset_mint.key())?;
        let accrual = &mut ctx.accounts.referral_accrual;
        if accrual.referral_partner == Pubkey::default() {
            accrual.initialize(
                ctx.accounts.referral_partner.key(),
                ctx.accounts.market.key(),
                ctx.accounts.asset_mint.key(),
                ctx.bumps.referral_accrual,
            )?;
        } else {
            require_keys_eq!(
                accrual.referral_partner,
                ctx.accounts.referral_partner.key(),
                ErrorCode::InvalidReferralAccrual
            );
            require_keys_eq!(
                accrual.market,
                ctx.accounts.market.key(),
                ErrorCode::InvalidReferralAccrual
            );
            require_keys_eq!(
                accrual.asset_mint,
                ctx.accounts.asset_mint.key(),
                ErrorCode::InvalidReferralAccrual
            );
        }
        Ok(())
    }
}
