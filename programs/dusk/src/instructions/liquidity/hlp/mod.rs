mod deposit_single_sided;
mod withdraw_single_sided;

use anchor_lang::prelude::*;

use crate::{
    constants::{FUTARCHY_AUTHORITY_SEED_PREFIX, MARKET_V2_SEED_PREFIX, YIELD_ACCOUNT_SEED_PREFIX},
    errors::ErrorCode,
    state::{FutarchyAuthority, HlpYieldEligibility, Market, MarketAsset, ProtocolAuctionSplit, YieldTokenKind},
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

    let futarchy_authority_bump = [futarchy_authority.bump];
    let expected_futarchy =
        Pubkey::create_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX, &futarchy_authority_bump], &crate::ID)
            .map_err(|_| error!(ErrorCode::InvalidFutarchyAuthority))?;
    require_keys_eq!(
        futarchy_authority_key,
        expected_futarchy,
        ErrorCode::InvalidFutarchyAuthority
    );
    Ok(())
}

pub(crate) fn validate_hlp_yield_account_pda(
    account_key: Pubkey,
    bump: u8,
    market_key: Pubkey,
    owner_key: Pubkey,
    hlp_mint: Pubkey,
    asset_mint: Pubkey,
) -> Result<()> {
    let bump_seed = [bump];
    let expected_account = Pubkey::create_program_address(
        &[
            YIELD_ACCOUNT_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            hlp_mint.as_ref(),
            asset_mint.as_ref(),
            &[YieldTokenKind::Hlp.code()],
            &bump_seed,
        ],
        &crate::ID,
    )
    .map_err(|_| error!(ErrorCode::InvalidYieldAccount))?;
    require_keys_eq!(account_key, expected_account, ErrorCode::InvalidYieldAccount);
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
    market: &mut Market,
    borrowed_asset: MarketAsset,
    actual_interest_credit: u64,
    protocol_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
    eligibility: HlpYieldEligibility,
) -> Result<()> {
    if actual_interest_credit == 0 {
        return Ok(());
    }

    require_eq!(
        market.base_side.shares.ylp_supply,
        market.quote_side.shares.ylp_supply,
        ErrorCode::BrokenInvariant
    );
    let frozen_non_hlp_supply = eligibility.non_hlp_ylp_supply()?;
    let current_non_hlp_supply = HlpYieldEligibility {
        ylp_supply: market.base_side.shares.ylp_supply,
        base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
        quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
    }
    .non_hlp_ylp_supply()?;
    require_eq!(
        current_non_hlp_supply,
        frozen_non_hlp_supply,
        ErrorCode::BrokenInvariant
    );

    // Every predictive mint/burn checkpoints the ownership interval that
    // precedes it. Flush any public-interest carry that materializes after
    // those mutations against the current shares before publishing this
    // source-specific credit; replaying frozen old shares here would
    // over-credit a vault that already burned them.
    market.checkpoint_hlp_yield_from_ylp(MarketAsset::Base)?;
    market.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote)?;

    market
        .side_mut(borrowed_asset)
        .record_hlp_funding_interest_credit_with_supply(
            actual_interest_credit,
            protocol_fee_bps,
            protocol_auction_split,
            frozen_non_hlp_supply,
        )?;

    // Advance both vaults across the hLP-funded index delta without crediting
    // either one. Zero eligible shares preserves their already-earned
    // sub-atom remainders while preventing self- and cross-rebates.
    market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, 0)?;
    market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, 0)
}

/// Publish one inline hLP interest-vault credit against ownership captured
/// before predictive or post-trade rebalancing. Funding interest publishes
/// only to the ordinary-plus-MIN lane; frozen hLP balances are excluded even
/// if the operation mints or burns vault-owned yLP before token settlement.
pub(crate) fn record_inline_hlp_interest_credit(
    market: &mut Market,
    borrowed_asset: MarketAsset,
    actual_interest_credit: u64,
    protocol_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
    eligibility: HlpYieldEligibility,
) -> Result<()> {
    record_hlp_interest_credit(
        market,
        borrowed_asset,
        actual_interest_credit,
        protocol_fee_bps,
        protocol_auction_split,
        eligibility,
    )
}

#[cfg(test)]
mod tests {
    include!("../../../tests/instructions/liquidity/hlp/mod.rs");
}
