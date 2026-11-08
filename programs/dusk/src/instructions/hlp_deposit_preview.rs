use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use super::{
    accounts::{require_supported_asset_mint, validate_lp_mint},
    reconcile_live_hlp_supply, validate_hlp_authority_pdas,
};
use crate::{
    errors::ErrorCode,
    state::{FutarchyAuthority, Market, MarketAsset},
    token::get_transfer_fee_for_epoch,
    transitions::liquidity::{
        current_hlp_curve_prices, current_hlp_entry_state_with_prices,
        prepare_concentrated_hlp_transition_at_current_state, HlpEntryDisposition,
    },
};

/// Every account is read-only. Admission checks and interest accrual affect
/// only deserialized memory, including when this instruction is submitted.
#[derive(Accounts)]
pub struct PreviewHlpDepositCapacity<'info> {
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlpDepositStatus {
    Ready,
    NotStarted,
    ReduceOnly,
    NoLiquidity,
    RebalanceRequired,
    CashConstrained,
    Unhedgeable,
    NoFundingCapacity,
    NoNetDeposit,
    Frozen,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlpDepositCapacityPreview {
    pub market: Pubkey,
    pub target_asset: MarketAsset,
    pub hlp_mint: Pubkey,
    pub slot: u64,
    pub epoch: u64,
    pub status: HlpDepositStatus,
    /// Exact funding ceiling, NOT a promise that all smaller amounts execute.
    /// Actual deposits still enforce rounding, settlement, custody and slippage.
    /// Gross wallet debit, including the current epoch's transfer fee.
    pub funding_limit_gross: u64,
    /// Greatest net target reserve credit supported by opposite-asset funding.
    pub funding_limit_net: u64,
}

impl<'info> PreviewHlpDepositCapacity<'info> {
    pub fn handle_preview(ctx: Context<Self>) -> Result<HlpDepositCapacityPreview> {
        let accounts = ctx.accounts;
        let market_key = accounts.market.key();
        validate_hlp_authority_pdas(
            &accounts.market,
            market_key,
            &accounts.futarchy_authority,
            accounts.futarchy_authority.key(),
        )?;
        require_keys_eq!(
            accounts.base_mint.key(),
            accounts.market.base_side.asset_mint,
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            accounts.quote_mint.key(),
            accounts.market.quote_side.asset_mint,
            ErrorCode::InvalidMint
        );
        require_supported_asset_mint(&accounts.base_mint)?;
        require_supported_asset_mint(&accounts.quote_mint)?;
        let target = accounts.market.asset_for_hlp_mint(accounts.target_hlp_mint.key())?;
        let target_mint = match target {
            MarketAsset::Base => &accounts.base_mint,
            MarketAsset::Quote => &accounts.quote_mint,
        };
        validate_lp_mint(&accounts.target_hlp_mint, market_key, target_mint.decimals)?;
        let clock = Clock::get()?;
        let (mut status, mut funding_limit_net) = preview_hlp_funding_limit(
            &mut accounts.market,
            target,
            accounts.target_hlp_mint.supply,
            accounts.futarchy_authority.global_reduce_only,
            &clock,
        )?;
        let funding_limit_gross = if funding_limit_net == 0 {
            0
        } else {
            // Net credit is monotone, including capped and 100% Token-2022
            // transfer fees. Inverse-fee helpers find the *smallest* gross
            // debit for a net value; this finds the exact upper boundary.
            gross_funding_limit(funding_limit_net, |amount| {
                get_transfer_fee_for_epoch(&target_mint.to_account_info(), amount, clock.epoch)
            })?
        };
        if status == HlpDepositStatus::Ready && funding_limit_gross == 0 {
            status = HlpDepositStatus::NoNetDeposit;
            funding_limit_net = 0;
        }
        Ok(HlpDepositCapacityPreview {
            market: market_key,
            target_asset: target,
            hlp_mint: accounts.target_hlp_mint.key(),
            slot: clock.slot,
            epoch: clock.epoch,
            status,
            funding_limit_gross,
            funding_limit_net,
        })
    }
}

fn gross_funding_limit(limit: u64, fee: impl Fn(u64) -> Result<u64>) -> Result<u64> {
    let net = |gross: u64| -> Result<u64> {
        gross
            .checked_sub(fee(gross)?)
            .ok_or_else(|| error!(ErrorCode::MarketMathOverflow))
    };
    let mut low = 0_u64;
    let mut high = u64::MAX;
    while low < high {
        let mid = low + (high - low) / 2 + 1;
        if net(mid)? <= limit {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Ok(if net(low)? == 0 { 0 } else { low })
}

fn preview_hlp_funding_limit(
    market: &mut Market,
    target: MarketAsset,
    live_supply: u64,
    global_reduce_only: bool,
    clock: &Clock,
) -> Result<(HlpDepositStatus, u64)> {
    market.assert_current_version()?;
    market.require_hlp_flash_idle(target)?;
    market.require_hlp_flash_idle(target.opposite())?;
    if clock.unix_timestamp < market.config.start_time {
        return Ok((HlpDepositStatus::NotStarted, 0));
    }
    if global_reduce_only || market.reduce_only {
        return Ok((HlpDepositStatus::ReduceOnly, 0));
    }
    market.accrue_interest_to_slot(clock.slot)?;
    reconcile_live_hlp_supply(market, target, live_supply)?;
    if market.base_side.reserves.live_reserve == 0 || market.quote_side.reserves.live_reserve == 0 {
        return Ok((HlpDepositStatus::NoLiquidity, 0));
    }
    market.advance_amm_clock(clock.slot)?;
    market.freeze_negative_hlp_vaults()?;
    if market.hlp_frozen(target) {
        return Ok((HlpDepositStatus::Frozen, 0));
    }
    if market.side(target.opposite()).reserves.cash_reserve == 0 {
        return Ok((HlpDepositStatus::NoFundingCapacity, 0));
    }
    // Entry settles the existing hedge atomically. Preview the same endpoint
    // before classifying readiness and calculating the remaining cash budget.
    prepare_concentrated_hlp_transition_at_current_state(market)?.consume(market)?;
    market.checkpoint_hlp_vaults()?;
    let prices = current_hlp_curve_prices(market)?;
    let entry = current_hlp_entry_state_with_prices(market, target, prices)?;
    let status = match entry.disposition {
        HlpEntryDisposition::Settled | HlpEntryDisposition::ControllerGranularityLimited => HlpDepositStatus::Ready,
        HlpEntryDisposition::Actionable => HlpDepositStatus::RebalanceRequired,
        HlpEntryDisposition::CashConstrained => HlpDepositStatus::CashConstrained,
        HlpEntryDisposition::Unhedgeable => HlpDepositStatus::Unhedgeable,
    };
    if status != HlpDepositStatus::Ready {
        return Ok((status, 0));
    }
    market.observe_current_risk(clock.slot)?;
    let target_reserve = market.side(target).reserves.live_reserve;
    let opposite_reserve = market.side(target.opposite()).reserves.live_reserve;
    if target_reserve == 0 || opposite_reserve == 0 {
        return Ok((HlpDepositStatus::NoLiquidity, 0));
    }
    let headroom = market.hlp_funding_headroom(target.opposite())?;
    // floor(deposit * opposite / target) <= headroom. Compare ratios first:
    // wider virtual reserves can overflow a u128 numerator even when the
    // final raw-token limit fits u64.
    let full_domain_fits = !crate::math::ratio_lte_full_width(
        u128::from(headroom) + 1,
        opposite_reserve,
        u64::MAX as u128,
        target_reserve,
    )?;
    let limit = if full_domain_fits {
        u64::MAX
    } else {
        let ceiling = crate::math::mul_div_ceil_u128(u128::from(headroom) + 1, target_reserve, opposite_reserve)?;
        u64::try_from(ceiling.saturating_sub(1)).map_err(|_| ErrorCode::MarketMathOverflow)?
    };
    if headroom == 0 || crate::math::mul_div_u128(u128::from(limit), opposite_reserve, target_reserve)? == 0 {
        return Ok((HlpDepositStatus::NoFundingCapacity, 0));
    }
    Ok((HlpDepositStatus::Ready, limit))
}

#[cfg(test)]
mod tests {
    include!("../tests/instructions/hlp_deposit_preview.rs");
}
