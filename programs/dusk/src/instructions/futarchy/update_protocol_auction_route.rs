use anchor_lang::prelude::*;

use crate::{
    constants::{FUTARCHY_AUTHORITY_SEED_PREFIX, MARKET_V2_SEED_PREFIX},
    errors::ErrorCode,
    events::ProtocolAuctionRouteUpdated,
    state::{FutarchyAuthority, Market, ProtocolAuctionLane},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateProtocolAuctionRouteArgs {
    pub lane: ProtocolAuctionLane,
    pub sold_mint: Pubkey,
    /// A governance-approved market key. The default key removes the approval
    /// and permits settlement only from the sold market's own direct pair.
    pub reference_market: Pubkey,
}

#[derive(Accounts)]
pub struct UpdateProtocolAuctionRoute<'info> {
    #[account(
        address = futarchy_authority.authority @ ErrorCode::InvalidFutarchyAuthority
    )]
    pub authority_signer: Signer<'info>,

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,
}

impl UpdateProtocolAuctionRoute<'_> {
    pub fn handle_update(ctx: Context<Self>, args: UpdateProtocolAuctionRouteArgs) -> Result<()> {
        ctx.accounts.futarchy_authority.validate()?;
        ctx.accounts.market.assert_started()?;
        let sold_side = ctx.accounts.market.asset_for_mint(args.sold_mint)?;
        ctx.accounts
            .market
            .side_mut(sold_side)
            .fees
            .set_protocol_auction_reference_market(args.lane, args.reference_market);

        emit!(ProtocolAuctionRouteUpdated {
            authority: ctx.accounts.futarchy_authority.key(),
            market: ctx.accounts.market.key(),
            lane: args.lane.code(),
            side: sold_side.code(),
            sold_mint: args.sold_mint,
            accepted_mint: ctx.accounts.futarchy_authority.auction_config(args.lane).accepted_mint,
            reference_market: args.reference_market,
            signer: ctx.accounts.authority_signer.key(),
        });
        Ok(())
    }
}
