use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, FUTARCHY_AUTHORITY_SEED_PREFIX, MAX_REFERRAL_INTEREST_SHARE_BPS},
    errors::ErrorCode,
    events::{ProtocolAuctionSplitUpdated, ReferralInterestShareCapUpdated},
    state::{FutarchyAuthority, ProtocolAuctionSplit, RevenueDistribution},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateProtocolRevenueArgs {
    pub swap_bps: Option<u16>,
    pub interest_bps: Option<u16>,
    pub max_referral_interest_share_bps: Option<u16>,
    pub revenue_distribution: Option<RevenueDistribution>,
    pub protocol_auction_split: Option<ProtocolAuctionSplit>,
}

#[event_cpi]
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
        let UpdateProtocolRevenue {
            authority_signer,
            futarchy_authority,
            ..
        } = ctx.accounts;

        if let Some(swap_bps) = args.swap_bps {
            require_gte!(BPS_DENOMINATOR, swap_bps, ErrorCode::InvalidSwapFeeBps);
            futarchy_authority.revenue_share.swap_bps = swap_bps;
        }
        if let Some(interest_bps) = args.interest_bps {
            require_gte!(BPS_DENOMINATOR, interest_bps, ErrorCode::InvalidInterestFeeBps);
            futarchy_authority.revenue_share.interest_bps = interest_bps;
        }

        if let Some(max_referral_interest_share_bps) = args.max_referral_interest_share_bps {
            require_gte!(
                MAX_REFERRAL_INTEREST_SHARE_BPS,
                max_referral_interest_share_bps,
                ErrorCode::InvalidReferralInterestShareBps
            );
            futarchy_authority.max_referral_interest_share_bps = max_referral_interest_share_bps;
            emit_cpi!(ReferralInterestShareCapUpdated {
                authority: futarchy_authority.key(),
                max_referral_interest_share_bps,
                signer: authority_signer.key(),
            });
        }

        if let Some(revenue_distribution) = args.revenue_distribution {
            require!(revenue_distribution.is_valid(), ErrorCode::InvalidDistribution);
            futarchy_authority.revenue_distribution = revenue_distribution;
        }

        if let Some(protocol_auction_split) = args.protocol_auction_split {
            require!(protocol_auction_split.is_valid(), ErrorCode::InvalidDistribution);
            futarchy_authority.protocol_auction_split = protocol_auction_split;
            emit_cpi!(ProtocolAuctionSplitUpdated {
                authority: futarchy_authority.key(),
                fee_auction_bps: protocol_auction_split.fee_auction_bps,
                buyback_auction_bps: protocol_auction_split.buyback_auction_bps,
                signer: authority_signer.key(),
            });
        }

        Ok(())
    }
}
