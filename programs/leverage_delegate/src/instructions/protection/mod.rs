use crate::{constants::*, errors::LeverageDelegateError, state::ProtectionOrder, token::*};
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_2022::Token2022,
    token_interface::{Mint, TokenAccount},
};
use dusk::{
    constants::{BPS_DENOMINATOR, LEVERAGE_MAINTENANCE_BUFFER_BPS},
    program::Dusk,
    state::{
        BorrowPosition, FutarchyAuthority, LeveragePosition, Market, MarketAsset, ReferralAccrual,
        ReferralPartner, YieldAccount, YieldTokenKind,
    },
};
mod create;
mod execute;
mod manage;
mod payment;
mod preview;
mod redeem;
pub use create::*;
pub use execute::*;
pub use manage::*;
pub use preview::*;

/// Repay borrow debt, add its opposite collateral, or repay leverage debt.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateProtectionOrderArgs {
    pub order_id: u64,
    pub action: u8,
    pub debt_asset: u8,
    pub lp_amount: u64,
    pub max_lp_per_execution: u64,
    pub max_payment_per_execution: u64,
    pub trigger_health_bps: u64,
    pub target_health_bps: u64,
    /// Minimum gross payment per LP smallest unit, scaled by 1e9.
    pub min_payment_per_lp_nad: u64,
    pub keeper_fee_bps: u16,
    pub expires_at: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ExecuteProtectionOrderArgs {
    pub lp_amount: u64,
    /// Maximum gross payment taken from the keeper's own token account.
    pub payment_amount: u64,
}

#[event]
pub struct ProtectionExecuted {
    pub order: Pubkey,
    pub position: Pubkey,
    pub keeper: Pubkey,
    pub lp_burned: u64,
    pub payment: u64,
    pub reward: u64,
    pub health_before_bps: u64,
    pub health_after_bps: u64,
}

#[inline(never)]
pub(super) fn protection_health(
    market_info: &AccountInfo,
    borrow: Option<&BorrowPosition>,
    leverage: Option<&LeveragePosition>,
    action: u8,
    asset: MarketAsset,
    clock: &Clock,
    refresh: bool,
) -> Result<u64> {
    // Fresh deserialization after CPIs avoids Anchor Account::reload's two
    // large Market temporaries sharing one 4 KiB SBF stack frame.
    let data = market_info.try_borrow_data()?;
    let mut cursor: &[u8] = &data;
    let mut decoded = unsafe {
        // The allocation is initialized exactly once before assume_init, and
        // Result retains any deserialization error without reading uninit data.
        let mut destination = Box::<Result<Market>>::new_uninit();
        destination
            .as_mut_ptr()
            .write(Market::try_deserialize(&mut cursor));
        destination.assume_init()
    };
    let market = decoded
        .as_mut()
        .as_mut()
        .map_err(|_| error!(LeverageDelegateError::InvalidOrder))?;
    if refresh {
        market.prepare_position_protection_snapshot(clock.slot)?;
    }
    if action < 2 {
        require!(leverage.is_none(), LeverageDelegateError::InvalidOrder);
        market
            .borrow_protection_health_bps(borrow.ok_or(LeverageDelegateError::InvalidOrder)?, asset)
    } else {
        require!(
            action == 2 && borrow.is_none(),
            LeverageDelegateError::InvalidOrder
        );
        let position = leverage.ok_or(LeverageDelegateError::InvalidOrder)?;
        require_eq!(
            position.debt_asset,
            asset.code(),
            LeverageDelegateError::InvalidOrder
        );
        let debt = position.debt_amount(&market.debt)?;
        if debt == 0 {
            return Ok(u64::MAX);
        }
        let value = market.leverage_protection_closeout_value(
            position,
            clock.slot,
            clock.unix_timestamp,
        )?;
        // One extra basis point makes this conservative with the liquidation
        // engine's integer-rounded equity test (equity_bps <= maintenance).
        Ok(
            ((value as u128) * (BPS_DENOMINATOR - LEVERAGE_MAINTENANCE_BUFFER_BPS - 1) as u128
                / debt as u128)
                .min(u64::MAX as u128) as u64,
        )
    }
}
