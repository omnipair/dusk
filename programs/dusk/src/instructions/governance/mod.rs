mod balance;
mod delegated;
pub use balance::*;
pub use delegated::*;
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
    state::{Market, YieldAccount, YieldTokenKind},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GovernanceYieldIndexes {
    pub base_swap_fee_q64: u128,
    pub base_interest_q64: u128,
    pub quote_swap_fee_q64: u128,
    pub quote_interest_q64: u128,
}

/// Close all yield intervals under the current parameters before changing an
/// external srLP balance or its matching virtual support balance.
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
    market: &Market,
    base_yield_account: &mut YieldAccount,
    quote_yield_account: &mut YieldAccount,
    live_srlp_balance: u64,
    indexes: GovernanceYieldIndexes,
) -> Result<()> {
    market.checkpoint_holder_income(base_yield_account, live_srlp_balance, None)?;
    market.checkpoint_holder_income(quote_yield_account, live_srlp_balance, None)?;
    base_yield_account.accrue(live_srlp_balance, indexes.base_swap_fee_q64, indexes.base_interest_q64)?;
    quote_yield_account.accrue(
        live_srlp_balance,
        indexes.quote_swap_fee_q64,
        indexes.quote_interest_q64,
    )
}

pub(crate) fn adjust_supporter_income_balance(
    base: &mut YieldAccount,
    quote: &mut YieldAccount,
    amount: u64,
    locking: bool,
) -> Result<()> {
    require_eq!(base.escrow_balance, quote.escrow_balance, ErrorCode::BrokenInvariant);
    let next = if locking {
        base.escrow_balance.checked_add(amount)
    } else {
        base.escrow_balance.checked_sub(amount)
    }
    .ok_or(ErrorCode::BrokenInvariant)?;
    base.escrow_balance = next;
    quote.escrow_balance = next;
    Ok(())
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
    srlp_mint: &InterfaceAccount<Mint>,
    base_hlp_srlp_vault: &InterfaceAccount<TokenAccount>,
    quote_hlp_srlp_vault: &InterfaceAccount<TokenAccount>,
) -> Result<()> {
    let market_key = market.key();
    validate_market_pda(market, market_key)?;
    require_keys_eq!(market.srlp_mint, srlp_mint.key(), ErrorCode::InvalidLpMintKey);
    validate_lp_mint(srlp_mint, market_key, market.base_side.asset_decimals)?;

    require_keys_eq!(
        market.base_hlp_vault.srlp_vault,
        base_hlp_srlp_vault.key(),
        ErrorCode::InvalidHlpVault
    );
    require_keys_eq!(base_hlp_srlp_vault.mint, srlp_mint.key(), ErrorCode::InvalidHlpVault);
    require_keys_eq!(base_hlp_srlp_vault.owner, market_key, ErrorCode::InvalidHlpVault);
    require_gte!(
        base_hlp_srlp_vault.amount,
        market.base_hlp_vault.srlp_shares,
        ErrorCode::InvalidHlpVault
    );

    require_keys_eq!(
        market.quote_hlp_vault.srlp_vault,
        quote_hlp_srlp_vault.key(),
        ErrorCode::InvalidHlpVault
    );
    require_keys_eq!(quote_hlp_srlp_vault.mint, srlp_mint.key(), ErrorCode::InvalidHlpVault);
    require_keys_eq!(quote_hlp_srlp_vault.owner, market_key, ErrorCode::InvalidHlpVault);
    require_gte!(
        quote_hlp_srlp_vault.amount,
        market.quote_hlp_vault.srlp_shares,
        ErrorCode::InvalidHlpVault
    );
    Ok(())
}

pub(crate) fn direct_srlp_eligible_supply(
    market: &Market,
    live_srlp_supply: u64,
    base_hlp_srlp_amount: u64,
    quote_hlp_srlp_amount: u64,
) -> Result<u64> {
    let total_ownership = live_srlp_supply
        .checked_add(market.governance_locked_srlp)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let senior = total_ownership
        .checked_sub(base_hlp_srlp_amount)
        .and_then(|amount| amount.checked_sub(quote_hlp_srlp_amount))
        .ok_or(ErrorCode::InvalidHlpVault)?;
    let active_junior_receipts = market
        .tranches
        .junior_receipt_supply
        .checked_sub(market.tranches.permanent_receipts)
        .and_then(|amount| amount.checked_sub(market.tranches.queued_receipts))
        .ok_or(ErrorCode::BrokenInvariant)?;
    let junior = crate::state::junior_receipt_value(active_junior_receipts, &market.tranches)?;
    senior
        .checked_add(junior)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

pub(crate) fn validate_supporter_accounts(
    market: &Account<Market>,
    supporter: Pubkey,
    srlp_mint: &InterfaceAccount<Mint>,
    supporter_srlp_account: &InterfaceAccount<TokenAccount>,
    base_yield_account: &YieldAccount,
    quote_yield_account: &YieldAccount,
) -> Result<()> {
    validate_owner_lp_account(supporter, srlp_mint, supporter_srlp_account)?;
    base_yield_account.assert_account(
        supporter,
        market.key(),
        srlp_mint.key(),
        market.base_side.asset_mint,
        YieldTokenKind::Srlp,
    )?;
    quote_yield_account.assert_account(
        supporter,
        market.key(),
        srlp_mint.key(),
        market.quote_side.asset_mint,
        YieldTokenKind::Srlp,
    )
}

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/governance/mod.rs");
}

/// Preserve proposal identity after the final direct or delegated allocation is released.
pub(super) fn save_released_proposal(
    proposal: &crate::state::ParameterProposal,
    info: &AccountInfo,
    proposer: &AccountInfo,
) -> Result<()> {
    if proposal.support_is_empty() {
        use crate::{account::get_size_with_discriminator, state::ParameterProposalTombstone};
        let space = get_size_with_discriminator::<ParameterProposalTombstone>();
        let refund = info
            .lamports()
            .checked_sub(Rent::get()?.minimum_balance(space))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        info.realloc(space, false)?;
        info.try_borrow_mut_data()?
            .copy_from_slice(ParameterProposalTombstone::DISCRIMINATOR);
        info.sub_lamports(refund)?;
        proposer.add_lamports(refund)?;
    } else {
        proposal.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
    }
    Ok(())
}
