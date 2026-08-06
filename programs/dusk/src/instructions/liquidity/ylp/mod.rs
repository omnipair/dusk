mod add_liquidity;
mod remove_liquidity;

use anchor_lang::prelude::*;

use crate::{
    constants::{MARKET_V2_SEED_PREFIX, YIELD_ACCOUNT_SEED_PREFIX},
    errors::ErrorCode,
    state::{Market, YieldTokenKind},
};

pub use add_liquidity::*;
pub use remove_liquidity::*;

pub(crate) fn validate_ylp_market_pda(market: &Market, market_key: Pubkey) -> Result<()> {
    let bump = [market.bump];
    let expected = Pubkey::create_program_address(
        &[
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
            &bump,
        ],
        &crate::ID,
    )
    .map_err(|_| error!(ErrorCode::InvalidMarket))?;
    require_keys_eq!(market_key, expected, ErrorCode::InvalidMarket);
    Ok(())
}

pub(crate) fn ylp_yield_account_pda(
    market_key: Pubkey,
    owner_key: Pubkey,
    ylp_mint: Pubkey,
    asset_mint: Pubkey,
) -> Result<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[
            YIELD_ACCOUNT_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            ylp_mint.as_ref(),
            asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        &crate::ID,
    )
    .ok_or_else(|| error!(ErrorCode::InvalidYieldAccount))
}
