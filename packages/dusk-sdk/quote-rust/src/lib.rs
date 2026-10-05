#![allow(unexpected_cfgs, dead_code, unused_imports)]

use anchor_lang::prelude::*;
use std::cell::RefCell;

// The shared leverage transition names the Token-2022 fee type for admission
// and liquidation helpers. Native close pricing invokes neither helper, so
// keep the WASM quote free of Token-2022's unrelated proof-generation graph.
// This local type makes those unused helpers compile; the SDK applies the
// effective transfer fee to each candidate before calling the quote export.
extern crate self as spl_token_2022;
pub mod extension {
    pub mod transfer_fee {
        #[derive(Clone, Copy, Debug, Default)]
        pub struct TransferFee {
            pub transfer_fee_basis_points: u16,
            pub maximum_fee: u64,
        }

        impl TransferFee {
            pub fn calculate_post_fee_amount(&self, gross: u64) -> Option<u64> {
                let proportional = (u128::from(gross)
                    .checked_mul(u128::from(self.transfer_fee_basis_points))?
                    .checked_add(9_999)?)
                    / 10_000;
                let fee = proportional.min(u128::from(self.maximum_fee));
                u64::try_from(u128::from(gross).checked_sub(fee)?).ok()
            }

            pub fn calculate_pre_fee_amount(&self, net: u64) -> Option<u64> {
                if net == 0 {
                    return Some(0);
                }
                let mut low = net.saturating_sub(1);
                let mut high = net.saturating_add(self.maximum_fee);
                if self.calculate_post_fee_amount(high)? < net {
                    return None;
                }
                while high - low > 1 {
                    let middle = low + (high - low) / 2;
                    if self.calculate_post_fee_amount(middle)? >= net {
                        high = middle;
                    } else {
                        low = middle;
                    }
                }
                Some(high)
            }
        }
    }
}

#[path = "../../../../programs/dusk/src/constants.rs"]
pub mod constants;
#[path = "../../../../programs/dusk/src/errors.rs"]
pub mod errors;
#[path = "../../../../programs/dusk/src/math/mod.rs"]
pub mod math;
#[path = "../../../../programs/dusk/src/state/mod.rs"]
pub mod state;
#[path = "../../../../programs/dusk/src/transitions/mod.rs"]
pub mod transitions;

declare_id!("JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X");

struct PreparedClose {
    market: state::Market,
    collateral_asset: state::MarketAsset,
    debt_amount: u64,
    timestamp: i64,
    slot: u64,
}

thread_local! {
    static PREPARED: RefCell<Option<PreparedClose>> = const { RefCell::new(None) };
    static LAST_AMOUNT_OUT: RefCell<u64> = const { RefCell::new(0) };
}

#[no_mangle]
pub extern "C" fn quote_alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len);
    let pointer = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    pointer
}

#[no_mangle]
pub unsafe extern "C" fn quote_free(pointer: *mut u8, len: usize) {
    if !pointer.is_null() {
        drop(Vec::from_raw_parts(pointer, 0, len));
    }
}

fn prepare(market_data: &[u8], position_data: &[u8], slot: u64, timestamp: i64) -> Result<PreparedClose> {
    let mut market = state::Market::try_deserialize(&mut &market_data[..])?;
    let position = state::LeveragePosition::try_deserialize(&mut &position_data[..])?;
    market.accrue_interest_to_slot(slot)?;
    let slice = market.leverage_close_slice(&position, constants::BPS_DENOMINATOR)?;
    market.prepare_amm_for_swap(slot)?;
    market.advance_one_amm_controller_target(slot)?;
    market.observe_current_risk(slot)?;
    let debt_asset = position.debt_asset()?;
    let debt_amount = market
        .debt
        .isolated_repayment_for_max(debt_asset, slice.debt_shares, u64::MAX)?
        .cash_repaid;
    Ok(PreparedClose {
        market,
        collateral_asset: debt_asset.opposite(),
        debt_amount,
        timestamp,
        slot,
    })
}

/// Returns 0 on success. Invalid account or market state returns -1.
#[no_mangle]
pub unsafe extern "C" fn quote_prepare(
    market_pointer: *const u8,
    market_len: usize,
    position_pointer: *const u8,
    position_len: usize,
    slot: u64,
    timestamp: i64,
) -> i32 {
    if market_pointer.is_null() || position_pointer.is_null() {
        return -1;
    }
    let market_data = std::slice::from_raw_parts(market_pointer, market_len);
    let position_data = std::slice::from_raw_parts(position_pointer, position_len);
    match prepare(market_data, position_data, slot, timestamp) {
        Ok(state) => {
            PREPARED.with(|prepared| *prepared.borrow_mut() = Some(state));
            0
        }
        Err(_) => -1,
    }
}

/// Returns 0 on success, 1 for a liquidity-limited swap, 2 for too little
/// input, or -1 on other errors.
#[no_mangle]
pub extern "C" fn quote_amount_out(collateral_in: u64) -> i32 {
    PREPARED.with(|prepared| {
        let borrow = prepared.borrow();
        let Some(state) = borrow.as_ref() else {
            return -1;
        };
        match state.market.quote_leverage_swap_at_time(
            state.collateral_asset,
            collateral_in,
            state.slot,
            state.timestamp,
        ) {
            Ok(quote) => {
                LAST_AMOUNT_OUT.with(|amount| *amount.borrow_mut() = quote.amount_out);
                0
            }
            Err(anchor_lang::error::Error::AnchorError(error))
                if error.error_code_number == u32::from(errors::ErrorCode::InsufficientLiquidity) =>
            {
                1
            }
            Err(anchor_lang::error::Error::AnchorError(error))
                if matches!(error.error_code_number,
                    code if code == u32::from(errors::ErrorCode::AmountZero)
                        || code == u32::from(errors::ErrorCode::InsufficientAmount)
                        || code == u32::from(errors::ErrorCode::InsufficientOutputAmount)
                ) =>
            {
                2
            }
            Err(_) => -1,
        }
    })
}

#[no_mangle]
pub extern "C" fn quote_last_amount_out() -> u64 {
    LAST_AMOUNT_OUT.with(|amount| *amount.borrow())
}

#[no_mangle]
pub extern "C" fn quote_debt_amount() -> u64 {
    PREPARED.with(|prepared| prepared.borrow().as_ref().map_or(0, |state| state.debt_amount))
}
