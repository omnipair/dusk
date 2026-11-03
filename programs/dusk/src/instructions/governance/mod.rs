mod balance;
mod delegated;
pub use balance::*;
pub use delegated::*;
mod proposal;
mod support;

pub use proposal::*;
pub use support::*;
mod manager;
pub use manager::*;

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
    market: &Market,
    base_yield_account: &mut YieldAccount,
    quote_yield_account: &mut YieldAccount,
    live_ylp_balance: u64,
    indexes: GovernanceYieldIndexes,
) -> Result<()> {
    market.checkpoint_holder_income(base_yield_account, live_ylp_balance, None)?;
    market.checkpoint_holder_income(quote_yield_account, live_ylp_balance, None)?;
    base_yield_account.accrue(live_ylp_balance, indexes.base_swap_fee_q64, indexes.base_interest_q64)?;
    quote_yield_account.accrue(live_ylp_balance, indexes.quote_swap_fee_q64, indexes.quote_interest_q64)
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
    let senior = total_ownership
        .checked_sub(base_hlp_ylp_amount)
        .and_then(|amount| amount.checked_sub(quote_hlp_ylp_amount))
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

/// New markets may not yet have hLP custody accounts. Their exact canonical
/// addresses can prove zero custody; initialized accounts retain full checks.
pub(crate) fn governance_eligible_from_accounts(
    market: &Account<Market>,
    ylp_mint: &InterfaceAccount<Mint>,
    base_hlp_ylp_vault: &UncheckedAccount,
    quote_hlp_ylp_vault: &UncheckedAccount,
) -> Result<u64> {
    validate_market_pda(market, market.key())?;
    require_keys_eq!(market.ylp_mint, ylp_mint.key(), ErrorCode::InvalidLpMintKey);
    validate_lp_mint(ylp_mint, market.key(), market.base_side.asset_decimals)?;
    let base = proposal::governance_vault_amount(
        market,
        ylp_mint,
        base_hlp_ylp_vault,
        market.base_hlp_vault.ylp_vault,
        market.base_hlp_vault.ylp_shares,
    )?;
    let quote = proposal::governance_vault_amount(
        market,
        ylp_mint,
        quote_hlp_ylp_vault,
        market.quote_hlp_vault.ylp_vault,
        market.quote_hlp_vault.ylp_shares,
    )?;
    direct_ylp_eligible_supply(market, ylp_mint.supply, base, quote)
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

/// Preserve proposal identity after the final direct or delegated allocation is released.
pub(super) fn save_released_proposal(
    proposal: &crate::state::ParameterProposal,
    info: &AccountInfo,
    proposer: &AccountInfo,
) -> Result<()> {
    if proposal.support_is_empty() && proposal.terminal() {
        use crate::{account::get_size_with_discriminator, state::ParameterProposalTombstone};
        let space = get_size_with_discriminator::<ParameterProposalTombstone>();
        let refund = info
            .lamports()
            .checked_sub(Rent::get()?.minimum_balance(space))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        info.realloc(space, false)?;
        ParameterProposalTombstone {
            market: proposal.market,
            proposer: proposal.proposer,
            nonce: proposal.nonce,
            digest: proposal.digest,
            status: proposal.status,
            bump: proposal.bump,
        }
        .try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
        info.sub_lamports(refund)?;
        proposer.add_lamports(refund)?;
    } else {
        proposal.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
    }
    Ok(())
}

pub(super) fn validate_governance_action(
    market: &Market,
    control: &crate::state::GovernanceControl,
    action: crate::state::GovernanceAction,
    updates: &[crate::state::MarketParameterUpdate],
) -> Result<()> {
    match action {
        crate::state::GovernanceAction::Parameters => {
            market.assert_launch_settings_unchanged_after_start(updates, Clock::get()?.unix_timestamp)?;
            market.validate_parameter_updates(updates)?;
        }
        crate::state::GovernanceAction::AppointManager {
            manager,
            expected_epoch,
        } => {
            require!(
                updates.is_empty() && manager != Pubkey::default(),
                ErrorCode::InvalidParameterProposal
            );
            require_eq!(expected_epoch, control.manager_epoch, ErrorCode::ManagerEpochMismatch);
        }
    }
    Ok(())
}

pub(super) fn effective_parameter_updates(
    market: &Market,
    updates: &[crate::state::MarketParameterUpdate],
) -> Vec<crate::state::MarketParameterUpdate> {
    updates
        .iter()
        .filter(|update| market.current_parameter_update(update.family()) != **update)
        .cloned()
        .collect()
}

pub(super) fn queue_supported_proposal(
    proposal: &mut crate::state::ParameterProposal,
    key: Pubkey,
    control: &mut crate::state::GovernanceControl,
    market: &Market,
    eligible_supply: u64,
    now: i64,
) -> Result<bool> {
    require!(
        proposal.revisions_current(&market.parameter_revisions),
        ErrorCode::ProposalStale
    );
    if !crate::state::has_strict_support_majority(proposal.current_support(&market.tranches)?, eligible_supply)? {
        return Ok(false);
    }
    match proposal.action {
        crate::state::GovernanceAction::Parameters => {
            market.assert_launch_settings_unchanged_after_start(&proposal.updates, now)?;
            let effective = effective_parameter_updates(market, &proposal.updates);
            if !effective.is_empty() {
                market.validate_parameter_updates(&effective)?;
            }
        }
        action => validate_governance_action(market, control, action, &proposal.updates)?,
    }
    let queued = proposal.queue_if_supported(eligible_supply, &market.tranches, now)?;
    proposal.seal_execution(&market.parameter_revisions)?;
    control.queue_lp(proposal.pending_bundle(key), &Clock::get()?)?;
    Ok(queued)
}

mod recovery;
pub use recovery::*;

pub(super) fn created_event(
    proposal: &crate::state::ParameterProposal,
    key: Pubkey,
    support: u64,
) -> crate::events::ParameterProposalCreated {
    crate::events::ParameterProposalCreated {
        proposal: key,
        market: proposal.market,
        proposer: proposal.proposer,
        nonce: proposal.nonce,
        origin: proposal.origin,
        action: proposal.action,
        conditions: proposal.conditions.clone(),
        execution_digest: proposal.execution_digest,
        junior_generation: proposal.junior_generation,
        family_revisions: proposal.family_revisions,
        digest: proposal.digest,
        sponsorship_floor: proposal.sponsorship_floor,
        initial_support: support,
        status: proposal.status.code(),
        updates: proposal.updates.clone(),
        metadata: proposal.metadata.clone(),
    }
}

pub(super) fn queued_event(
    proposal: &crate::state::ParameterProposal,
    key: Pubkey,
) -> crate::events::ParameterProposalQueued {
    crate::events::ParameterProposalQueued {
        proposal: key,
        clock: proposal.origin.clock_kind(),
        execution_digest: proposal.execution_digest,
        family_revisions: proposal.family_revisions,
        total_locked: proposal.queued_support,
        eligible_supply: proposal.queued_eligible_ylp,
        queued_at: proposal.queued_at,
        execute_after: proposal.execute_after,
        execution_deadline: proposal.execution_deadline,
    }
}

mod manager_submit;
pub use manager_submit::*;
