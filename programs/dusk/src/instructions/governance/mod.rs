mod proposal;
mod support;

pub use proposal::*;
pub use support::*;

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    instructions::accounts::{validate_lp_mint, validate_owner_lp_account},
    state::{Market, ParameterFamily, YieldAccount, YieldTokenKind},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GovernanceYieldIndexes {
    pub base_swap_fee_q64: u128,
    pub base_interest_q64: u128,
    pub quote_swap_fee_q64: u128,
    pub quote_interest_q64: u128,
}

pub(crate) fn current_parameter_revision(market: &Market, family: ParameterFamily) -> u64 {
    market.parameter_revisions[family.code() as usize]
}

/// Close all yield intervals under the current parameters before changing an
/// external yLP balance or its matching virtual support balance.
pub(crate) fn carry_forward_governance_yield(market: &mut Market, current_slot: u64) -> Result<GovernanceYieldIndexes> {
    market.accrue_interest_to_slot(current_slot)?;
    market.base_side.carry_forward_swap_fees()?;
    market.base_side.carry_forward_interest()?;
    market.quote_side.carry_forward_swap_fees()?;
    market.quote_side.carry_forward_interest()?;
    Ok(GovernanceYieldIndexes {
        base_swap_fee_q64: market.base_side.fees.swap_fee_growth_index_q64,
        base_interest_q64: market.base_side.fees.interest_growth_index_q64,
        quote_swap_fee_q64: market.quote_side.fees.swap_fee_growth_index_q64,
        quote_interest_q64: market.quote_side.fees.interest_growth_index_q64,
    })
}

pub(crate) fn checkpoint_supporter_yield(
    base_yield_account: &mut YieldAccount,
    quote_yield_account: &mut YieldAccount,
    live_ylp_balance: u64,
    indexes: GovernanceYieldIndexes,
) -> Result<()> {
    base_yield_account.accrue(live_ylp_balance, indexes.base_swap_fee_q64, indexes.base_interest_q64)?;
    quote_yield_account.accrue(live_ylp_balance, indexes.quote_swap_fee_q64, indexes.quote_interest_q64)
}

pub(crate) fn validate_market_pda(market: &Market, market_key: Pubkey) -> Result<()> {
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

pub(crate) fn validate_governance_token_accounts(
    market: &Account<Market>,
    ylp_mint: &InterfaceAccount<Mint>,
    base_hlp_ylp_vault: &InterfaceAccount<TokenAccount>,
    quote_hlp_ylp_vault: &InterfaceAccount<TokenAccount>,
) -> Result<()> {
    let market_key = market.key();
    validate_market_pda(market, market_key)?;
    require_keys_eq!(market.ylp_mint, ylp_mint.key(), ErrorCode::InvalidLpMintKey);
    validate_lp_mint(ylp_mint, market_key, market.base_side.asset_decimals)?;

    require_keys_eq!(
        market.base_hlp_vault.ylp_vault,
        base_hlp_ylp_vault.key(),
        ErrorCode::InvalidHlpVault
    );
    require_keys_eq!(base_hlp_ylp_vault.mint, ylp_mint.key(), ErrorCode::InvalidHlpVault);
    require_keys_eq!(base_hlp_ylp_vault.owner, market_key, ErrorCode::InvalidHlpVault);
    require_gte!(
        base_hlp_ylp_vault.amount,
        market.base_hlp_vault.ylp_shares,
        ErrorCode::InvalidHlpVault
    );

    require_keys_eq!(
        market.quote_hlp_vault.ylp_vault,
        quote_hlp_ylp_vault.key(),
        ErrorCode::InvalidHlpVault
    );
    require_keys_eq!(quote_hlp_ylp_vault.mint, ylp_mint.key(), ErrorCode::InvalidHlpVault);
    require_keys_eq!(quote_hlp_ylp_vault.owner, market_key, ErrorCode::InvalidHlpVault);
    require_gte!(
        quote_hlp_ylp_vault.amount,
        market.quote_hlp_vault.ylp_shares,
        ErrorCode::InvalidHlpVault
    );
    Ok(())
}

pub(crate) fn direct_ylp_eligible_supply(
    market: &Market,
    live_ylp_supply: u64,
    base_hlp_ylp_amount: u64,
    quote_hlp_ylp_amount: u64,
) -> Result<u64> {
    let total_ownership = live_ylp_supply
        .checked_add(market.governance_locked_ylp)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    total_ownership
        .checked_sub(base_hlp_ylp_amount)
        .and_then(|amount| amount.checked_sub(quote_hlp_ylp_amount))
        .ok_or_else(|| ErrorCode::InvalidHlpVault.into())
}

pub(crate) fn validate_supporter_accounts(
    market: &Account<Market>,
    supporter: Pubkey,
    ylp_mint: &InterfaceAccount<Mint>,
    supporter_ylp_account: &InterfaceAccount<TokenAccount>,
    base_yield_account: &YieldAccount,
    quote_yield_account: &YieldAccount,
) -> Result<()> {
    validate_owner_lp_account(supporter, ylp_mint, supporter_ylp_account)?;
    base_yield_account.assert_account(
        supporter,
        market.key(),
        ylp_mint.key(),
        market.base_side.asset_mint,
        YieldTokenKind::Ylp,
    )?;
    quote_yield_account.assert_account(
        supporter,
        market.key(),
        ylp_mint.key(),
        market.quote_side.asset_mint,
        YieldTokenKind::Ylp,
    )
}

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/governance/mod.rs");
}
