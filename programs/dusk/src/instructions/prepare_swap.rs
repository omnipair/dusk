use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{
    get_processed_sibling_instruction, get_stack_height, Instruction, TRANSACTION_LEVEL_STACK_HEIGHT,
};
use anchor_lang::solana_program::sysvar::instructions::{
    load_current_index_checked, load_instruction_at_checked, ID as INSTRUCTIONS_SYSVAR_ID,
};
use anchor_lang::Discriminator;

use crate::{
    errors::ErrorCode,
    state::{Market, MarketAsset},
};

pub(crate) fn enforce_launch_same_transaction_guard(
    market: &Market,
    market_key: Pubkey,
    asset_in: MarketAsset,
    unix_timestamp: i64,
    instructions_sysvar: &AccountInfo<'_>,
    allow_direct_cpi: bool,
) -> Result<()> {
    if !market
        .config
        .launch_rate_limit_active_for_swap(asset_in, unix_timestamp)
    {
        return Ok(());
    }
    require_keys_eq!(
        *instructions_sysvar.key,
        INSTRUCTIONS_SYSVAR_ID,
        ErrorCode::LaunchRateLimitSplitTransaction
    );
    // External flash settlement moves repayment only. Count the paired settle
    // as a price-moving action only when it carries both reserve vaults for
    // the internal AMM route; otherwise an ordinary Dusk swap inside the
    // flash route would be incorrectly treated as a split launch trade.
    let touches_market = |ix: &Instruction| ix.accounts.iter().any(|meta| meta.pubkey == market_key);
    let is_action = |ix: &Instruction| {
        if ix.program_id != crate::ID || !touches_market(ix) {
            return false;
        }
        let Some(discriminator) = ix.data.get(..8) else {
            return false;
        };
        discriminator == crate::instruction::Swap::DISCRIMINATOR
            || discriminator == crate::instruction::OpenLeverage::DISCRIMINATOR
            || discriminator == crate::instruction::IncreaseLeverage::DISCRIMINATOR
            || discriminator == crate::instruction::DecreaseLeverage::DISCRIMINATOR
            || discriminator == crate::instruction::CloseLeverage::DISCRIMINATOR
            || discriminator == crate::instruction::DelegatedCloseLeverage::DISCRIMINATOR
            || discriminator == crate::instruction::EmergencyLiquidation::DISCRIMINATOR
            || (ix.data.get(..8) == Some(crate::instruction::SettleFlashLiquidation::DISCRIMINATOR)
                && ix.accounts.iter().any(|m| m.pubkey == market.base_side.reserve_vault)
                && ix.accounts.iter().any(|m| m.pubkey == market.quote_side.reserve_vault))
    };
    let current_index = usize::from(
        load_current_index_checked(instructions_sysvar).map_err(|_| ErrorCode::LaunchRateLimitSplitTransaction)?,
    );
    let current = load_instruction_at_checked(current_index, instructions_sysvar)
        .map_err(|_| ErrorCode::LaunchRateLimitSplitTransaction)?;
    let via_cpi = current.program_id != crate::ID;
    require!(
        if via_cpi {
            allow_direct_cpi && get_stack_height() == TRANSACTION_LEVEL_STACK_HEIGHT + 1 && touches_market(&current)
        } else {
            is_action(&current)
        },
        ErrorCode::LaunchRateLimitSplitTransaction
    );

    if via_cpi {
        // At this depth, earlier calls under the same router are siblings.
        for index in 0.. {
            let Some(sibling) = get_processed_sibling_instruction(index) else {
                break;
            };
            require!(!is_action(&sibling), ErrorCode::LaunchRateLimitSplitTransaction);
        }
    }

    // A different top-level router instruction can hide its own CPI.
    // For a routed buy, reject other external instructions that can write
    // this market, in addition to the classified Dusk actions.
    for index in 0.. {
        let Ok(instruction) = load_instruction_at_checked(index, instructions_sysvar) else {
            break;
        };
        if index != current_index {
            let other_router = via_cpi
                && instruction.program_id != crate::ID
                && instruction
                    .accounts
                    .iter()
                    .any(|meta| meta.pubkey == market_key && meta.is_writable);
            require!(
                !is_action(&instruction) && !other_router,
                ErrorCode::LaunchRateLimitSplitTransaction
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../tests/instructions/prepare_swap.rs");
}
