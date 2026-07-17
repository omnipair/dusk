use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, FUTARCHY_AUTHORITY_SEED_PREFIX, MAX_REFERRAL_ORIGINATION_FEE_BPS},
    errors::ErrorCode,
    events::{ProtocolAuctionSplitUpdated, ReferralOriginationFeeUpdated},
    state::{FutarchyAuthority, ProtocolAuctionSplit, RevenueDistribution},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateProtocolRevenueArgs {
    pub swap_bps: Option<u16>,
    pub interest_bps: Option<u16>,
    pub referral_origination_fee_bps: Option<u16>,
    pub revenue_distribution: Option<RevenueDistribution>,
    pub protocol_auction_split: Option<ProtocolAuctionSplit>,
}

#[derive(Accounts)]
pub struct UpdateProtocolRevenue<'info> {
    #[account(
        mut,
        address = futarchy_authority.authority @ ErrorCode::InvalidFutarchyAuthority
    )]
    pub authority_signer: Signer<'info>,

    #[account(
        mut,
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
}

impl<'info> UpdateProtocolRevenue<'info> {
    pub fn handle_update(ctx: Context<Self>, args: UpdateProtocolRevenueArgs) -> Result<()> {
        if let Some(swap_bps) = args.swap_bps {
            require_gte!(BPS_DENOMINATOR, swap_bps, ErrorCode::InvalidSwapFeeBps);
            ctx.accounts.futarchy_authority.revenue_share.swap_bps = swap_bps;
        }
        if let Some(interest_bps) = args.interest_bps {
            require_gte!(BPS_DENOMINATOR, interest_bps, ErrorCode::InvalidInterestFeeBps);
            ctx.accounts.futarchy_authority.revenue_share.interest_bps = interest_bps;
        }
        if let Some(referral_origination_fee_bps) = args.referral_origination_fee_bps {
            require_gte!(
                MAX_REFERRAL_ORIGINATION_FEE_BPS,
                referral_origination_fee_bps,
                ErrorCode::InvalidReferralFeeBps
            );
            ctx.accounts.futarchy_authority.referral_origination_fee_bps = referral_origination_fee_bps;
            emit!(ReferralOriginationFeeUpdated {
                authority: ctx.accounts.futarchy_authority.key(),
                referral_origination_fee_bps,
                signer: ctx.accounts.authority_signer.key(),
            });
        }
        if let Some(revenue_distribution) = args.revenue_distribution {
            require!(revenue_distribution.is_valid(), ErrorCode::InvalidDistribution);
            ctx.accounts.futarchy_authority.revenue_distribution = revenue_distribution;
        }
        if let Some(protocol_auction_split) = args.protocol_auction_split {
            require!(protocol_auction_split.is_valid(), ErrorCode::InvalidDistribution);
            ctx.accounts.futarchy_authority.protocol_auction_split = protocol_auction_split;
            emit!(ProtocolAuctionSplitUpdated {
                authority: ctx.accounts.futarchy_authority.key(),
                fee_auction_bps: protocol_auction_split.fee_auction_bps,
                buyback_auction_bps: protocol_auction_split.buyback_auction_bps,
                signer: ctx.accounts.authority_signer.key(),
            });
        }
        Ok(())
    }
}
