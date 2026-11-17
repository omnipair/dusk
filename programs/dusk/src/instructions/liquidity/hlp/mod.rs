mod burn_retired_receipt;
mod deposit_single_sided;
mod rotate_cohort;
mod withdraw_single_sided;

use anchor_lang::prelude::*;

use crate::{
    constants::{FUTARCHY_AUTHORITY_SEED_PREFIX, MARKET_V2_SEED_PREFIX, YIELD_ACCOUNT_SEED_PREFIX},
    errors::ErrorCode,
    state::{FutarchyAuthority, Market, MarketAsset, YieldTokenKind},
};

pub use burn_retired_receipt::*;
pub use deposit_single_sided::*;
pub use rotate_cohort::*;
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
/// exit. Historical nested srLP growth is published against the old stored
/// denominator first; only future growth and principal pricing use the smaller
/// live supply.
///
/// After a complete direct burn no receipt remains to authorize the vault's
/// final exit. A terminally closed vault holds no value, so its stored supply
/// simply follows the mint to zero. A live vault is unwound only by the
/// orphan close (`withdraw_single_sided` for the whole stored supply, signed
/// by anyone), which keeps the stored supply for that exit; every other entry
/// and exit rejects until then.
pub(crate) fn reconcile_live_hlp_supply(
    market: &mut Market,
    target_asset: MarketAsset,
    live_mint_supply: u64,
    orphan_close: bool,
) -> Result<()> {
    market.checkpoint_hlp_yield_from_srlp(target_asset)?;
    let terminal = market.hlp_terminally_closed(target_asset);
    let stored_supply = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault.hlp_supply,
        MarketAsset::Quote => &mut market.quote_hlp_vault.hlp_supply,
    };
    require_gte!(*stored_supply, live_mint_supply, ErrorCode::InvalidHlpMintSupply);
    if *stored_supply > 0 && live_mint_supply == 0 {
        require!(terminal || orphan_close, ErrorCode::InvalidHlpMintSupply);
        if orphan_close {
            return Ok(());
        }
    }
    if live_mint_supply < *stored_supply {
        *stored_supply = live_mint_supply;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../../../tests/instructions/liquidity/hlp/mod.rs");
}
