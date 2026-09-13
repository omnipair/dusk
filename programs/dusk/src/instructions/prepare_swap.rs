use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::{
    load_current_index_checked, load_instruction_at_checked, ID as INSTRUCTIONS_SYSVAR_ID,
};
use anchor_lang::Discriminator;

use crate::{
    errors::ErrorCode,
    state::{Market, MarketAsset},
    transitions::HlpRebalanceReceipt,
};

pub(crate) fn enforce_launch_same_transaction_guard(
    market: &Market,
    market_key: Pubkey,
    asset_in: MarketAsset,
    unix_timestamp: i64,
    instructions_sysvar: &AccountInfo<'_>,
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
    let current_index = usize::from(
        load_current_index_checked(instructions_sysvar).map_err(|_| ErrorCode::LaunchRateLimitSplitTransaction)?,
    );
    let current = load_instruction_at_checked(current_index, instructions_sysvar)
        .map_err(|_| ErrorCode::LaunchRateLimitSplitTransaction)?;
    require!(
        current.program_id == crate::ID
            && launch_price_moving_instruction(&current.data)
            && current.accounts.iter().any(|meta| meta.pubkey == market_key),
        ErrorCode::LaunchRateLimitSplitTransaction
    );

    let mut matching_market_actions = 0_u8;
    let mut index = 0_usize;
    while let Ok(instruction) = load_instruction_at_checked(index, instructions_sysvar) {
        if instruction.program_id == crate::ID
            && launch_price_moving_instruction(&instruction.data)
            && instruction.accounts.iter().any(|meta| meta.pubkey == market_key)
        {
            matching_market_actions = matching_market_actions
                .checked_add(1)
                .ok_or(ErrorCode::LaunchRateLimitSplitTransaction)?;
            require!(matching_market_actions <= 1, ErrorCode::LaunchRateLimitSplitTransaction);
        }
        index = index.checked_add(1).ok_or(ErrorCode::LaunchRateLimitSplitTransaction)?;
    }
    require_eq!(matching_market_actions, 1, ErrorCode::LaunchRateLimitSplitTransaction);
    Ok(())
}

fn launch_price_moving_instruction(data: &[u8]) -> bool {
    let Some(discriminator) = data.get(..8) else {
        return false;
    };
    discriminator == crate::instruction::Swap::DISCRIMINATOR
        || discriminator == crate::instruction::OpenLeverage::DISCRIMINATOR
        || discriminator == crate::instruction::IncreaseLeverage::DISCRIMINATOR
        || discriminator == crate::instruction::DecreaseLeverage::DISCRIMINATOR
        || discriminator == crate::instruction::CloseLeverage::DISCRIMINATOR
        || discriminator == crate::instruction::LiquidateLeveragePosition::DISCRIMINATOR
        || discriminator == crate::instruction::BackstopLiquidationAuction::DISCRIMINATOR
        || discriminator == crate::instruction::RescueHlp::DISCRIMINATOR
}

pub(crate) fn rebalance_executes_token_changes(receipt: &HlpRebalanceReceipt) -> bool {
    receipt.ylp_mint_amount > 0 || receipt.ylp_burn_amount > 0 || receipt.interest_paid > 0
}

#[cfg(test)]
mod tests {
    include!("../tests/instructions/prepare_swap.rs");
}
