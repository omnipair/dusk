use anchor_lang::solana_program::log::sol_log_data;
use anchor_lang::{prelude::*, Discriminator};

use super::{HlpClosed, HlpOpened, HlpRebalanced, MarketHealthUpdated, PositionLiquidated, SwapExecuted, SwapSettled};

const MARKET_EVENT_METADATA_LEN: usize = 32 + 32 + 8;

pub(crate) fn emit_swap_settled_low_heap(
    market: Pubkey,
    trader: Pubkey,
    asset_in_side: u8,
    reserve_credit: u64,
    amount_in_after_fee: u64,
    amount_out: u64,
    fee_credit: u64,
    base_hlp_pending_rebalance: i128,
    quote_hlp_pending_rebalance: i128,
) {
    const SWAP_SETTLED_EVENT_LEN: usize = 8 + 32 + 32 + 1 + 8 + 8 + 8 + 8 + 16 + 16;

    let mut data = [0u8; SWAP_SETTLED_EVENT_LEN];
    let mut offset = 0usize;
    data[offset..offset + 8].copy_from_slice(SwapSettled::DISCRIMINATOR);
    offset += 8;
    data[offset..offset + 32].copy_from_slice(market.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(trader.as_ref());
    offset += 32;
    data[offset] = asset_in_side;
    offset += 1;
    data[offset..offset + 8].copy_from_slice(&reserve_credit.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&amount_in_after_fee.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&amount_out.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&fee_credit.to_le_bytes());
    offset += 8;
    data[offset..offset + 16].copy_from_slice(&base_hlp_pending_rebalance.to_le_bytes());
    offset += 16;
    data[offset..offset + 16].copy_from_slice(&quote_hlp_pending_rebalance.to_le_bytes());

    sol_log_data(&[&data]);
}

pub(crate) fn emit_swap_executed_low_heap(
    market: Pubkey,
    trader: Pubkey,
    asset_in_mint: Pubkey,
    asset_out_mint: Pubkey,
    reserve_credit: u64,
    amount_in_after_fee: u64,
    amount_out: u64,
    fee_credit: u64,
    base_hlp_pending_rebalance: i128,
    quote_hlp_pending_rebalance: i128,
    slot: u64,
) {
    const SWAP_EXECUTED_EVENT_LEN: usize = 8 + 32 + 32 + 32 + 32 + 8 + 8 + 8 + 8 + 16 + 16 + MARKET_EVENT_METADATA_LEN;

    let mut data = [0u8; SWAP_EXECUTED_EVENT_LEN];
    let mut offset = 0usize;
    data[offset..offset + 8].copy_from_slice(SwapExecuted::DISCRIMINATOR);
    offset += 8;
    data[offset..offset + 32].copy_from_slice(market.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(trader.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(asset_in_mint.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(asset_out_mint.as_ref());
    offset += 32;
    data[offset..offset + 8].copy_from_slice(&reserve_credit.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&amount_in_after_fee.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&amount_out.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&fee_credit.to_le_bytes());
    offset += 8;
    data[offset..offset + 16].copy_from_slice(&base_hlp_pending_rebalance.to_le_bytes());
    offset += 16;
    data[offset..offset + 16].copy_from_slice(&quote_hlp_pending_rebalance.to_le_bytes());
    offset += 16;
    write_market_event_metadata(&mut data, offset, trader, market, slot);

    sol_log_data(&[&data]);
}

pub(crate) fn emit_hlp_rebalanced_low_heap(
    market: Pubkey,
    signer: Pubkey,
    target_side: u8,
    ideal_delta: i128,
    executed_delta: i128,
    pending_rebalance: i128,
    nav_nad: u128,
    slot: u64,
) {
    const HLP_REBALANCED_EVENT_LEN: usize = 8 + 32 + 1 + 16 + 16 + 16 + 16 + MARKET_EVENT_METADATA_LEN;

    let mut data = [0u8; HLP_REBALANCED_EVENT_LEN];
    let mut offset = 0usize;
    data[offset..offset + 8].copy_from_slice(HlpRebalanced::DISCRIMINATOR);
    offset += 8;
    data[offset..offset + 32].copy_from_slice(market.as_ref());
    offset += 32;
    data[offset] = target_side;
    offset += 1;
    data[offset..offset + 16].copy_from_slice(&ideal_delta.to_le_bytes());
    offset += 16;
    data[offset..offset + 16].copy_from_slice(&executed_delta.to_le_bytes());
    offset += 16;
    data[offset..offset + 16].copy_from_slice(&pending_rebalance.to_le_bytes());
    offset += 16;
    data[offset..offset + 16].copy_from_slice(&nav_nad.to_le_bytes());
    offset += 16;
    write_market_event_metadata(&mut data, offset, signer, market, slot);

    sol_log_data(&[&data]);
}

pub(crate) fn emit_market_health_updated_low_heap(
    market: Pubkey,
    signer: Pubkey,
    global_health_base_contribution_for_quote_debt: u64,
    global_health_quote_contribution_for_base_debt: u64,
    effective_base_debt_nad: u128,
    effective_quote_debt_nad: u128,
    base_debt_health_bps: u64,
    quote_debt_health_bps: u64,
    slot: u64,
) {
    const MARKET_HEALTH_UPDATED_EVENT_LEN: usize = 8 + 32 + 8 + 8 + 16 + 16 + 8 + 8 + MARKET_EVENT_METADATA_LEN;

    let mut data = [0u8; MARKET_HEALTH_UPDATED_EVENT_LEN];
    let mut offset = 0usize;
    data[offset..offset + 8].copy_from_slice(MarketHealthUpdated::DISCRIMINATOR);
    offset += 8;
    data[offset..offset + 32].copy_from_slice(market.as_ref());
    offset += 32;
    data[offset..offset + 8].copy_from_slice(&global_health_base_contribution_for_quote_debt.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&global_health_quote_contribution_for_base_debt.to_le_bytes());
    offset += 8;
    data[offset..offset + 16].copy_from_slice(&effective_base_debt_nad.to_le_bytes());
    offset += 16;
    data[offset..offset + 16].copy_from_slice(&effective_quote_debt_nad.to_le_bytes());
    offset += 16;
    data[offset..offset + 8].copy_from_slice(&base_debt_health_bps.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&quote_debt_health_bps.to_le_bytes());
    offset += 8;
    write_market_event_metadata(&mut data, offset, signer, market, slot);

    sol_log_data(&[&data]);
}

pub(crate) fn emit_hlp_opened_low_heap(
    market: Pubkey,
    owner: Pubkey,
    asset_mint: Pubkey,
    deposit_amount: u64,
    borrowed_amount: u64,
    ylp_amount: u64,
    hlp_amount: u64,
    hlp_supply: u64,
) -> Result<()> {
    const HLP_OPENED_EVENT_LEN: usize = 8 + (3 * 32) + (5 * 8) + MARKET_EVENT_METADATA_LEN;

    let mut data = [0u8; HLP_OPENED_EVENT_LEN];
    let mut offset = 0usize;
    data[offset..offset + 8].copy_from_slice(HlpOpened::DISCRIMINATOR);
    offset += 8;
    data[offset..offset + 32].copy_from_slice(market.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(owner.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(asset_mint.as_ref());
    offset += 32;
    data[offset..offset + 8].copy_from_slice(&deposit_amount.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&borrowed_amount.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&ylp_amount.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&hlp_amount.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&hlp_supply.to_le_bytes());
    offset += 8;
    write_market_event_metadata(&mut data, offset, owner, market, Clock::get()?.slot);

    sol_log_data(&[&data]);
    Ok(())
}

pub(crate) fn emit_hlp_closed_low_heap(
    market: Pubkey,
    owner: Pubkey,
    asset_mint: Pubkey,
    hlp_amount: u64,
    ylp_amount: u64,
    target_amount_out: u64,
    debt_repaid: u64,
    interest_paid: u64,
    hlp_supply: u64,
) -> Result<()> {
    const HLP_CLOSED_EVENT_LEN: usize = 8 + (3 * 32) + (6 * 8) + MARKET_EVENT_METADATA_LEN;

    let mut data = [0u8; HLP_CLOSED_EVENT_LEN];
    let mut offset = 0usize;
    data[offset..offset + 8].copy_from_slice(HlpClosed::DISCRIMINATOR);
    offset += 8;
    data[offset..offset + 32].copy_from_slice(market.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(owner.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(asset_mint.as_ref());
    offset += 32;
    data[offset..offset + 8].copy_from_slice(&hlp_amount.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&ylp_amount.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&target_amount_out.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&debt_repaid.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&interest_paid.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&hlp_supply.to_le_bytes());
    offset += 8;
    write_market_event_metadata(&mut data, offset, owner, market, Clock::get()?.slot);

    sol_log_data(&[&data]);
    Ok(())
}

pub(crate) fn emit_position_liquidated_low_heap(
    market: Pubkey,
    borrow_position: Pubkey,
    borrower: Pubkey,
    liquidator: Pubkey,
    debt_asset_mint: Pubkey,
    collateral_asset_mint: Pubkey,
    repaid_amount: u64,
    collateral_seized: u64,
    collateral_to_liquidator: u64,
    insurance_funded: u64,
    insurance_drawn: u64,
    socialized_loss: u64,
    remaining_debt: u128,
    remaining_global_health_contribution: u64,
    remaining_liquidation_cf_bps: u16,
) -> Result<()> {
    const POSITION_LIQUIDATED_EVENT_LEN: usize = 8 + (6 * 32) + (7 * 8) + 16 + 2 + MARKET_EVENT_METADATA_LEN;

    let mut data = [0u8; POSITION_LIQUIDATED_EVENT_LEN];
    let mut offset = 0usize;
    data[offset..offset + 8].copy_from_slice(PositionLiquidated::DISCRIMINATOR);
    offset += 8;
    data[offset..offset + 32].copy_from_slice(market.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(borrow_position.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(borrower.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(liquidator.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(debt_asset_mint.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(collateral_asset_mint.as_ref());
    offset += 32;
    data[offset..offset + 8].copy_from_slice(&repaid_amount.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&collateral_seized.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&collateral_to_liquidator.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&insurance_funded.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&insurance_drawn.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&socialized_loss.to_le_bytes());
    offset += 8;
    data[offset..offset + 16].copy_from_slice(&remaining_debt.to_le_bytes());
    offset += 16;
    data[offset..offset + 8].copy_from_slice(&remaining_global_health_contribution.to_le_bytes());
    offset += 8;
    data[offset..offset + 2].copy_from_slice(&remaining_liquidation_cf_bps.to_le_bytes());
    offset += 2;
    write_market_event_metadata(&mut data, offset, liquidator, market, Clock::get()?.slot);

    sol_log_data(&[&data]);
    Ok(())
}

fn write_market_event_metadata(data: &mut [u8], offset: usize, signer: Pubkey, market: Pubkey, slot: u64) {
    let mut cursor = offset;
    data[cursor..cursor + 32].copy_from_slice(signer.as_ref());
    cursor += 32;
    data[cursor..cursor + 32].copy_from_slice(market.as_ref());
    cursor += 32;
    data[cursor..cursor + 8].copy_from_slice(&slot.to_le_bytes());
}
