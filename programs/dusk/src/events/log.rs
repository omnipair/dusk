use anchor_lang::solana_program::log::sol_log_data;
use anchor_lang::{prelude::*, Discriminator};

use super::{
    HlpClosed, HlpOpened, HlpRebalanced, MarketHealthUpdated, PositionLiquidated, SwapExecuted, SwapFeeBreakdownEvent,
    SwapSettled,
};
use crate::state::{AmmSwapQuote, SwapFeeBreakdown, SwapReceipt};

const MARKET_EVENT_METADATA_LEN: usize = 32 + 32 + 8;
const SWAP_FEE_BREAKDOWN_EVENT_LEN: usize = 15 * 8;
const SWAP_QUOTE_TELEMETRY_LEN: usize = SWAP_FEE_BREAKDOWN_EVENT_LEN + (7 * 8);
const SWAP_SETTLED_EVENT_LEN: usize = 8 + 32 + 32 + 1 + (4 * 8) + (2 * 16) + SWAP_QUOTE_TELEMETRY_LEN;
const SWAP_EXECUTED_EVENT_LEN: usize =
    8 + (4 * 32) + (4 * 8) + (2 * 16) + MARKET_EVENT_METADATA_LEN + SWAP_QUOTE_TELEMETRY_LEN;

pub(crate) fn emit_swap_settled_low_heap(
    market: Pubkey,
    trader: Pubkey,
    asset_in_side: u8,
    quote: &AmmSwapQuote,
    receipt: &SwapReceipt,
    base_hlp_pending_rebalance: i128,
    quote_hlp_pending_rebalance: i128,
) {
    let data = serialize_swap_settled_low_heap(
        market,
        trader,
        asset_in_side,
        quote,
        receipt,
        base_hlp_pending_rebalance,
        quote_hlp_pending_rebalance,
    );
    sol_log_data(&[&data]);
}

fn serialize_swap_settled_low_heap(
    market: Pubkey,
    trader: Pubkey,
    asset_in_side: u8,
    quote: &AmmSwapQuote,
    receipt: &SwapReceipt,
    base_hlp_pending_rebalance: i128,
    quote_hlp_pending_rebalance: i128,
) -> [u8; SWAP_SETTLED_EVENT_LEN] {
    let mut data = [0_u8; SWAP_SETTLED_EVENT_LEN];
    let mut offset = 0usize;
    data[offset..offset + 8].copy_from_slice(SwapSettled::DISCRIMINATOR);
    offset += 8;
    data[offset..offset + 32].copy_from_slice(market.as_ref());
    offset += 32;
    data[offset..offset + 32].copy_from_slice(trader.as_ref());
    offset += 32;
    data[offset] = asset_in_side;
    offset += 1;
    data[offset..offset + 8].copy_from_slice(&quote.fee.reserve_credit.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&receipt.amount_in_after_fee.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&receipt.amount_out.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&receipt.fee_credit.to_le_bytes());
    offset += 8;
    data[offset..offset + 16].copy_from_slice(&base_hlp_pending_rebalance.to_le_bytes());
    offset += 16;
    data[offset..offset + 16].copy_from_slice(&quote_hlp_pending_rebalance.to_le_bytes());
    offset += 16;
    write_swap_quote_telemetry(&mut data, offset, quote, receipt);
    data
}

pub(crate) fn emit_swap_executed_low_heap(
    market: Pubkey,
    trader: Pubkey,
    asset_in_mint: Pubkey,
    asset_out_mint: Pubkey,
    quote: &AmmSwapQuote,
    receipt: &SwapReceipt,
    base_hlp_pending_rebalance: i128,
    quote_hlp_pending_rebalance: i128,
    slot: u64,
) {
    let data = serialize_swap_executed_low_heap(
        market,
        trader,
        asset_in_mint,
        asset_out_mint,
        quote,
        receipt,
        base_hlp_pending_rebalance,
        quote_hlp_pending_rebalance,
        slot,
    );
    sol_log_data(&[&data]);
}

#[allow(clippy::too_many_arguments)]
fn serialize_swap_executed_low_heap(
    market: Pubkey,
    trader: Pubkey,
    asset_in_mint: Pubkey,
    asset_out_mint: Pubkey,
    quote: &AmmSwapQuote,
    receipt: &SwapReceipt,
    base_hlp_pending_rebalance: i128,
    quote_hlp_pending_rebalance: i128,
    slot: u64,
) -> [u8; SWAP_EXECUTED_EVENT_LEN] {
    let mut data = [0_u8; SWAP_EXECUTED_EVENT_LEN];
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
    data[offset..offset + 8].copy_from_slice(&quote.fee.reserve_credit.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&receipt.amount_in_after_fee.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&receipt.amount_out.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&receipt.fee_credit.to_le_bytes());
    offset += 8;
    data[offset..offset + 16].copy_from_slice(&base_hlp_pending_rebalance.to_le_bytes());
    offset += 16;
    data[offset..offset + 16].copy_from_slice(&quote_hlp_pending_rebalance.to_le_bytes());
    offset += 16;
    write_market_event_metadata(&mut data, offset, trader, market, slot);
    offset += MARKET_EVENT_METADATA_LEN;
    write_swap_quote_telemetry(&mut data, offset, quote, receipt);
    data
}

fn write_swap_quote_telemetry(data: &mut [u8], mut offset: usize, quote: &AmmSwapQuote, receipt: &SwapReceipt) {
    write_swap_fee_breakdown(data, &mut offset, &quote.fee);
    write_u64(data, &mut offset, quote.start_price_nad);
    write_u64(data, &mut offset, quote.end_price_nad);
    write_u64(data, &mut offset, quote.reserve_end_price_nad);
    write_u64(data, &mut offset, quote.decayed_volatility_nad);
    write_u64(data, &mut offset, quote.post_success_volatility_nad);
    write_u64(data, &mut offset, receipt.base_fee_credit);
    write_u64(data, &mut offset, receipt.distributed_surcharge_credit);
}

fn write_swap_fee_breakdown(data: &mut [u8], offset: &mut usize, fee: &SwapFeeBreakdown) {
    write_u64(data, offset, fee.reserve_credit);
    write_u64(data, offset, fee.base_fee_debit);
    write_u64(data, offset, fee.divergence_surcharge_debit);
    write_u64(data, offset, fee.volatility_surcharge_debit);
    write_u64(data, offset, fee.dynamic_surcharge_debit);
    write_u64(data, offset, fee.total_fee_debit);
    write_u64(data, offset, fee.retained_surcharge);
    write_u64(data, offset, fee.distributed_surcharge_debit);
    write_u64(data, offset, fee.amount_in_for_quote);
    write_u64(data, offset, fee.reserve_input_credit);
    write_u64(data, offset, fee.claimable_fee_debit);
    write_u64(data, offset, fee.base_fee_rate_nad);
    write_u64(data, offset, fee.divergence_fee_rate_nad);
    write_u64(data, offset, fee.volatility_fee_rate_nad);
    write_u64(data, offset, fee.total_fee_rate_nad);
}

fn write_u64(data: &mut [u8], offset: &mut usize, value: u64) {
    data[*offset..*offset + 8].copy_from_slice(&value.to_le_bytes());
    *offset += 8;
}

impl From<SwapFeeBreakdown> for SwapFeeBreakdownEvent {
    fn from(value: SwapFeeBreakdown) -> Self {
        Self {
            reserve_credit: value.reserve_credit,
            base_fee_debit: value.base_fee_debit,
            divergence_surcharge_debit: value.divergence_surcharge_debit,
            volatility_surcharge_debit: value.volatility_surcharge_debit,
            dynamic_surcharge_debit: value.dynamic_surcharge_debit,
            total_fee_debit: value.total_fee_debit,
            retained_surcharge: value.retained_surcharge,
            distributed_surcharge_debit: value.distributed_surcharge_debit,
            amount_in_for_quote: value.amount_in_for_quote,
            reserve_input_credit: value.reserve_input_credit,
            claimable_fee_debit: value.claimable_fee_debit,
            base_fee_rate_nad: value.base_fee_rate_nad,
            divergence_fee_rate_nad: value.divergence_fee_rate_nad,
            volatility_fee_rate_nad: value.volatility_fee_rate_nad,
            total_fee_rate_nad: value.total_fee_rate_nad,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::MarketEventMetadata,
        state::{MarketAsset, SwapFeeBreakdown},
    };

    fn fixture() -> (AmmSwapQuote, SwapReceipt) {
        let fee = SwapFeeBreakdown {
            reserve_credit: 1_000,
            base_fee_debit: 3,
            divergence_surcharge_debit: 5,
            volatility_surcharge_debit: 7,
            dynamic_surcharge_debit: 12,
            total_fee_debit: 15,
            retained_surcharge: 12,
            distributed_surcharge_debit: 0,
            amount_in_for_quote: 985,
            reserve_input_credit: 997,
            claimable_fee_debit: 3,
            base_fee_rate_nad: 30_000,
            divergence_fee_rate_nad: 50_000,
            volatility_fee_rate_nad: 70_000,
            total_fee_rate_nad: 150_000,
        };
        let quote = AmmSwapQuote::new_uncertified(MarketAsset::Base, 911, 1_001, 1_002, 1_005, 1_003, 1_004, fee);
        let receipt = SwapReceipt {
            amount_in_after_fee: fee.amount_in_for_quote,
            reserve_input_credit: fee.reserve_input_credit,
            amount_out: quote.amount_out,
            fee_credit: 2,
            base_fee_credit: 2,
            distributed_surcharge_credit: 0,
            fee_breakdown: fee,
            ..SwapReceipt::default()
        };
        (quote, receipt)
    }

    #[test]
    fn swap_settled_low_heap_matches_anchor_serialization() {
        let market = Pubkey::new_unique();
        let trader = Pubkey::new_unique();
        let (quote, receipt) = fixture();
        let base_pending = -17;
        let quote_pending = 19;
        let data = serialize_swap_settled_low_heap(
            market,
            trader,
            MarketAsset::Base.code(),
            &quote,
            &receipt,
            base_pending,
            quote_pending,
        );
        let event = SwapSettled {
            market,
            trader,
            asset_in_side: MarketAsset::Base.code(),
            reserve_credit: quote.fee.reserve_credit,
            amount_in_after_fee: receipt.amount_in_after_fee,
            amount_out: receipt.amount_out,
            fee_credit: receipt.fee_credit,
            base_hlp_pending_rebalance: base_pending,
            quote_hlp_pending_rebalance: quote_pending,
            fee_breakdown: quote.fee.into(),
            start_price_nad: quote.start_price_nad,
            end_price_nad: quote.end_price_nad,
            reserve_end_price_nad: quote.reserve_end_price_nad,
            decayed_volatility_nad: quote.decayed_volatility_nad,
            post_success_volatility_nad: quote.post_success_volatility_nad,
            base_fee_credit: receipt.base_fee_credit,
            distributed_surcharge_credit: receipt.distributed_surcharge_credit,
        };
        let mut expected = SwapSettled::DISCRIMINATOR.to_vec();
        expected.extend(event.try_to_vec().unwrap());
        assert_eq!(data.len(), SWAP_SETTLED_EVENT_LEN);
        assert_eq!(data.as_slice(), expected.as_slice());
    }

    #[test]
    fn swap_executed_low_heap_matches_anchor_serialization() {
        let market = Pubkey::new_unique();
        let trader = Pubkey::new_unique();
        let asset_in_mint = Pubkey::new_unique();
        let asset_out_mint = Pubkey::new_unique();
        let slot = 77;
        let (quote, receipt) = fixture();
        let base_pending = -23;
        let quote_pending = 29;
        let data = serialize_swap_executed_low_heap(
            market,
            trader,
            asset_in_mint,
            asset_out_mint,
            &quote,
            &receipt,
            base_pending,
            quote_pending,
            slot,
        );
        let event = SwapExecuted {
            market,
            trader,
            asset_in_mint,
            asset_out_mint,
            reserve_credit: quote.fee.reserve_credit,
            amount_in_after_fee: receipt.amount_in_after_fee,
            amount_out: receipt.amount_out,
            fee_credit: receipt.fee_credit,
            base_hlp_pending_rebalance: base_pending,
            quote_hlp_pending_rebalance: quote_pending,
            metadata: MarketEventMetadata {
                signer: trader,
                market,
                slot,
            },
            fee_breakdown: quote.fee.into(),
            start_price_nad: quote.start_price_nad,
            end_price_nad: quote.end_price_nad,
            reserve_end_price_nad: quote.reserve_end_price_nad,
            decayed_volatility_nad: quote.decayed_volatility_nad,
            post_success_volatility_nad: quote.post_success_volatility_nad,
            base_fee_credit: receipt.base_fee_credit,
            distributed_surcharge_credit: receipt.distributed_surcharge_credit,
        };
        let mut expected = SwapExecuted::DISCRIMINATOR.to_vec();
        expected.extend(event.try_to_vec().unwrap());
        assert_eq!(data.len(), SWAP_EXECUTED_EVENT_LEN);
        assert_eq!(data.as_slice(), expected.as_slice());
    }
}
