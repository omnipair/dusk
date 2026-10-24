use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, FUTARCHY_AUTHORITY_SEED_PREFIX, MARKET_V2_SEED_PREFIX},
    errors::ErrorCode,
    events::{ProtocolAuctionConfigUpdated, ProtocolAuctionRecipientsUpdated, ProtocolAuctionRouteUpdated},
    state::{FutarchyAuthority, Market, ProtocolAuctionLane, ProtocolAuctionParams},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateProtocolAuctionConfigArgs {
    pub lane: ProtocolAuctionLane,
    pub accepted_mint: Option<Pubkey>,
    pub params: Option<ProtocolAuctionParams>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct UpdateProtocolAuctionConfig<'info> {
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

impl<'info> UpdateProtocolAuctionConfig<'info> {
    pub fn handle_update(ctx: Context<Self>, args: UpdateProtocolAuctionConfigArgs) -> Result<()> {
        let UpdateProtocolAuctionConfig {
            authority_signer,
            futarchy_authority,
            ..
        } = ctx.accounts;
        let lane = args.lane;
        let authority = futarchy_authority.key();
        let signer = authority_signer.key();
        let auction = futarchy_authority.auction_config_mut(lane);

        if let Some(accepted_mint) = args.accepted_mint {
            require_keys_neq!(accepted_mint, Pubkey::default(), ErrorCode::InvalidMint);
            auction.accepted_mint = accepted_mint;
        }
        if let Some(params) = args.params {
            params.validate()?;
            auction.params = params;
        }

        auction.validate()?;
        let accepted_mint = auction.accepted_mint;
        let params = auction.params;

        emit_cpi!(ProtocolAuctionConfigUpdated {
            authority,
            lane: lane.code(),
            accepted_mint,
            start_multiplier_bps: params.start_multiplier_bps,
            floor_multiplier_bps: params.floor_multiplier_bps,
            duration_slots: params.duration_slots,
            max_reference_age_slots: params.max_reference_age_slots,
            signer,
        });

        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateProtocolAuctionRecipientsArgs {
    pub lane: ProtocolAuctionLane,
    pub treasury: Option<Pubkey>,
    pub staking_vault: Option<Pubkey>,
    pub treasury_bps: Option<u16>,
    pub staking_vault_bps: Option<u16>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct UpdateProtocolAuctionRecipients<'info> {
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

impl<'info> UpdateProtocolAuctionRecipients<'info> {
    pub fn handle_update(ctx: Context<Self>, args: UpdateProtocolAuctionRecipientsArgs) -> Result<()> {
        let UpdateProtocolAuctionRecipients {
            authority_signer,
            futarchy_authority,
            ..
        } = ctx.accounts;
        let lane = args.lane;
        let authority = futarchy_authority.key();
        let signer = authority_signer.key();
        let auction = futarchy_authority.auction_config_mut(lane);

        if let Some(treasury) = args.treasury {
            auction.recipients.treasury = treasury;
        }
        if let Some(staking_vault) = args.staking_vault {
            auction.recipients.staking_vault = staking_vault;
        }
        if let Some(treasury_bps) = args.treasury_bps {
            require_gte!(BPS_DENOMINATOR, treasury_bps, ErrorCode::InvalidDistribution);
            auction.recipients.treasury_bps = treasury_bps;
        }
        if let Some(staking_vault_bps) = args.staking_vault_bps {
            require_gte!(BPS_DENOMINATOR, staking_vault_bps, ErrorCode::InvalidDistribution);
            auction.recipients.staking_vault_bps = staking_vault_bps;
        }

        require!(auction.recipients.is_valid(), ErrorCode::InvalidDistribution);
        let recipients = auction.recipients;

        emit_cpi!(ProtocolAuctionRecipientsUpdated {
            authority,
            lane: lane.code(),
            treasury: recipients.treasury,
            staking_vault: recipients.staking_vault,
            treasury_bps: recipients.treasury_bps,
            staking_vault_bps: recipients.staking_vault_bps,
            signer,
        });

        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateProtocolAuctionRouteArgs {
    pub lane: ProtocolAuctionLane,
    pub sold_mint: Pubkey,
    /// A governance-approved market key. The default key removes the approval
    /// and permits settlement only from the sold market's own direct pair.
    pub reference_market: Pubkey,
}

#[event_cpi]
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
        let UpdateProtocolAuctionRoute {
            authority_signer,
            futarchy_authority,
            market,
            ..
        } = ctx.accounts;

        futarchy_authority.validate()?;
        market.assert_started()?;
        let sold_side = market.asset_for_mint(args.sold_mint)?;
        market
            .side_mut(sold_side)
            .fees
            .set_protocol_auction_reference_market(args.lane, args.reference_market);

        emit_cpi!(ProtocolAuctionRouteUpdated {
            authority: futarchy_authority.key(),
            market: market.key(),
            lane: args.lane.code(),
            side: sold_side.code(),
            sold_mint: args.sold_mint,
            accepted_mint: futarchy_authority.auction_config(args.lane).accepted_mint,
            reference_market: args.reference_market,
            signer: authority_signer.key(),
        });

        Ok(())
    }
}
