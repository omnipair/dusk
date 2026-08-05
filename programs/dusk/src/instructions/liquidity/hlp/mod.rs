mod deposit_single_sided;
mod withdraw_single_sided;

use anchor_lang::prelude::*;

use crate::{
    constants::{FUTARCHY_AUTHORITY_SEED_PREFIX, MARKET_V2_SEED_PREFIX},
    errors::ErrorCode,
    state::{FutarchyAuthority, HlpYieldEligibility, Market, MarketAsset, MarketSide, ProtocolAuctionSplit},
};

pub use deposit_single_sided::*;
pub use withdraw_single_sided::*;

/// Validate the two program authorities outside Anchor's generated account
/// parser. Constructing Market-derived seed arrays while the large Market
/// deserialization frame is live can exceed SBF's 4 KiB stack limit.
pub(crate) fn validate_hlp_authority_pdas(
    market: &Market,
    market_key: Pubkey,
    futarchy_authority: &FutarchyAuthority,
    futarchy_authority_key: Pubkey,
) -> Result<()> {
    let market_bump = [market.bump];
    let expected_market = Pubkey::create_program_address(
        &[
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
            &market_bump,
        ],
        &crate::ID,
    )
    .map_err(|_| error!(ErrorCode::InvalidMarket))?;
    require_keys_eq!(market_key, expected_market, ErrorCode::InvalidMarket);

    let futarchy_bump = [futarchy_authority.bump];
    let expected_futarchy =
        Pubkey::create_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX, &futarchy_bump], &crate::ID)
            .map_err(|_| error!(ErrorCode::InvalidFutarchyAuthority))?;
    require_keys_eq!(
        futarchy_authority_key,
        expected_futarchy,
        ErrorCode::InvalidFutarchyAuthority
    );
    Ok(())
}

/// Reconcile a partial direct Token-2022 hLP burn before the next hLP entry or
/// exit. Historical nested yLP growth is published against the old stored
/// denominator first; only future growth and principal pricing use the smaller
/// live supply. A complete direct burn is intentionally fail-closed because no
/// holder remains who can authorize the vault's normal final exit.
pub(crate) fn reconcile_live_hlp_supply(
    market: &mut Market,
    target_asset: MarketAsset,
    live_mint_supply: u64,
) -> Result<()> {
    market.checkpoint_hlp_yield_from_ylp(target_asset)?;
    let stored_supply = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault.hlp_supply,
        MarketAsset::Quote => &mut market.quote_hlp_vault.hlp_supply,
    };
    require_gte!(*stored_supply, live_mint_supply, ErrorCode::InvalidHlpMintSupply);
    if *stored_supply > 0 {
        require!(live_mint_supply > 0, ErrorCode::InvalidHlpMintSupply);
    }
    if live_mint_supply < *stored_supply {
        *stored_supply = live_mint_supply;
    }
    Ok(())
}

pub(crate) fn record_hlp_interest_credit(
    borrowed_side: &mut MarketSide,
    actual_interest_credit: u64,
    manager_fee_bps: u16,
    protocol_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
    eligible_ylp_supply: u64,
) -> Result<()> {
    borrowed_side.record_interest_credit_with_supply(
        actual_interest_credit,
        manager_fee_bps,
        protocol_fee_bps,
        protocol_auction_split,
        0,
        eligible_ylp_supply,
    )?;
    Ok(())
}

/// Publish one inline hLP interest-vault credit against ownership captured
/// before predictive or post-trade rebalancing. Both hLP vaults own ordinary
/// yLP and therefore both must receive their historical share of either
/// asset's interest stream.
pub(crate) fn record_inline_hlp_interest_credit(
    market: &mut Market,
    borrowed_asset: MarketAsset,
    actual_interest_credit: u64,
    manager_fee_bps: u16,
    protocol_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
    eligibility: HlpYieldEligibility,
) -> Result<()> {
    record_hlp_interest_credit(
        market.side_mut(borrowed_asset),
        actual_interest_credit,
        manager_fee_bps,
        protocol_fee_bps,
        protocol_auction_split,
        eligibility.ylp_supply,
    )?;
    market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, eligibility.base_hlp_ylp_shares)?;
    market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, eligibility.quote_hlp_ylp_shares)
}

#[cfg(test)]
mod tests {
    include!("../../../tests/instructions/liquidity/hlp/mod.rs");
}
