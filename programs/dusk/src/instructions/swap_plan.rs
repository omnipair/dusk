use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    state::{
        market::transitions::hedge::{
            hlp_curve_prices_from_base_price_nad, pre_solve_one_hlp_for_swap, require_residual_hlp_swap_safe,
        },
        AmmSwapQuote, HlpRebalanceReceipt, HlpYieldEligibility, Market, MarketAsset, SwapFeeBreakdown,
    },
};

/// All state-derived inputs frozen for one swap quote. Execution and preview
/// construct the same context after validating their instruction accounts and
/// reading `Clock` once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SwapContext {
    pub current_slot: u64,
    pub asset_in: MarketAsset,
    pub reserve_credit: u64,
    /// Capacity that must remain available for an explicit borrow committed
    /// after this swap plan. Automatic hLP positioning may use only the
    /// remainder. The reservation is denominated in `asset_in` atoms.
    pub reserved_daily_borrow: u64,
}

/// State-only plan shared by preview and execution. Token transfers and the
/// post-trade hLP correction remain execution concerns.
#[derive(Debug)]
pub(crate) struct SwapPlan {
    pub quote: AmmSwapQuote,
    pub base_pre_rebalance: HlpRebalanceReceipt,
    pub quote_pre_rebalance: HlpRebalanceReceipt,
    pub fee_eligible_ylp_supply: u64,
    pub interest_eligibility: HlpYieldEligibility,
}

impl SwapContext {
    pub(crate) fn plan(self, market: &mut Market) -> Result<SwapPlan> {
        // Snapshot actionable remainders before predictive positioning. The
        // pre-solver deliberately creates temporary exposure against this
        // operation's expected endpoint; that is not stale exposure and must
        // not make the same operation fail its settlement-band guard.
        let base_hlp_residual_on_entry = market.base_hlp_vault.residual_exposure != 0;
        let quote_hlp_residual_on_entry = market.quote_hlp_vault.residual_exposure != 0;
        market.accrue_interest_to_slot(self.current_slot)?;
        require_eq!(
            market.base_side.shares.ylp_supply,
            market.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        let interest_eligibility = HlpYieldEligibility {
            ylp_supply: market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
        };
        if market.base_side.reserves.live_reserve > 0 && market.quote_side.reserves.live_reserve > 0 {
            market.prepare_amm_for_swap(self.current_slot)?;
        }

        // The safety reference spans the whole user operation. A funded ramp
        // or recenter can move the curve before the trader quote, so using the
        // post-controller quote start would let that movement hide a net
        // worsening of residual hLP exposure.
        let has_active_hlp = market.has_active_hlp();
        let operation_start_price_nad = has_active_hlp
            .then(|| market.curve_marginal_price_nad(self.current_slot))
            .transpose()?;
        market.advance_one_amm_controller_target(self.current_slot)?;
        let pre_state = market.dynamic_fee_pre_state(self.current_slot)?;
        let preliminary =
            market.preliminary_swap_inputs_for_state(self.reserve_credit, self.current_slot, pre_state)?;

        let (base_pre_rebalance, quote_pre_rebalance, fee_eligible_ylp_supply) = if has_active_hlp {
            require_gte!(
                preliminary.reserve_input_credit,
                preliminary.amount_in_for_quote,
                ErrorCode::BrokenInvariant
            );
            let base = pre_solve_one_hlp_for_swap(
                market,
                MarketAsset::Base,
                self.asset_in,
                preliminary.amount_in_for_quote,
                preliminary.reserve_input_credit,
                self.current_slot,
                self.asset_in,
                self.reserved_daily_borrow,
            )?;
            let quote = pre_solve_one_hlp_for_swap(
                market,
                MarketAsset::Quote,
                self.asset_in,
                preliminary.amount_in_for_quote,
                preliminary.reserve_input_credit,
                self.current_slot,
                self.asset_in,
                self.reserved_daily_borrow,
            )?;
            let pre_solve_ylp_mint_amount = base
                .ylp_mint_amount
                .checked_add(quote.ylp_mint_amount)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let eligible_supply = market
                .side(self.asset_in)
                .shares
                .ylp_supply
                .checked_sub(pre_solve_ylp_mint_amount)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            (base, quote, eligible_supply)
        } else {
            // With no hLP supply or actionable residual there is nothing to
            // predict. The same preliminary fee state is still reused by the
            // authoritative quote below, so it is evaluated exactly once.
            (
                HlpRebalanceReceipt {
                    target_asset: MarketAsset::Base,
                    ..HlpRebalanceReceipt::default()
                },
                HlpRebalanceReceipt {
                    target_asset: MarketAsset::Quote,
                    ..HlpRebalanceReceipt::default()
                },
                market.side(self.asset_in).shares.ylp_supply,
            )
        };

        if hlp_receipt_mutates_curve_inventory(&base_pre_rebalance)
            || hlp_receipt_mutates_curve_inventory(&quote_pre_rebalance)
        {
            market.checkpoint_amm_neutral_inventory(self.current_slot)?;
        } else {
            market.ensure_amm_initialized(self.current_slot)?;
        }
        let quote = market.quote_amm_swap_for_reserves_nad(
            self.asset_in,
            self.reserve_credit,
            self.current_slot,
            market.curve_reserves_nad()?,
            pre_state,
            preliminary,
        )?;
        if let Some(operation_start_price_nad) = operation_start_price_nad {
            let start_prices = hlp_curve_prices_from_base_price_nad(operation_start_price_nad as u128)?;
            let end_prices = hlp_curve_prices_from_base_price_nad(quote.reserve_end_price_nad as u128)?;
            require_residual_hlp_swap_safe(
                market,
                MarketAsset::Base,
                start_prices,
                end_prices,
                base_hlp_residual_on_entry,
            )?;
            require_residual_hlp_swap_safe(
                market,
                MarketAsset::Quote,
                start_prices,
                end_prices,
                quote_hlp_residual_on_entry,
            )?;
        }

        Ok(SwapPlan {
            quote,
            base_pre_rebalance,
            quote_pre_rebalance,
            fee_eligible_ylp_supply,
            interest_eligibility,
        })
    }
}

pub(crate) fn hlp_receipt_mutates_curve_inventory(receipt: &HlpRebalanceReceipt) -> bool {
    receipt.executed_delta != 0
        || receipt.ylp_mint_amount != 0
        || receipt.ylp_burn_amount != 0
        || receipt.debt_delta != 0
        || receipt.interest_paid != 0
}

pub(crate) fn split_claimable_fee_credit(fee: &SwapFeeBreakdown, total_credit: u64) -> Result<(u64, u64)> {
    require_eq!(
        fee.base_fee_debit
            .checked_add(fee.distributed_surcharge_debit)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        fee.claimable_fee_debit,
        ErrorCode::BrokenInvariant
    );
    require_gte!(fee.claimable_fee_debit, total_credit, ErrorCode::BrokenInvariant);
    if fee.claimable_fee_debit == 0 {
        require_eq!(total_credit, 0, ErrorCode::BrokenInvariant);
        return Ok((0, 0));
    }
    let base_credit = u64::try_from(
        (total_credit as u128)
            .checked_mul(fee.base_fee_debit as u128)
            .and_then(|value| value.checked_div(fee.claimable_fee_debit as u128))
            .ok_or(ErrorCode::FeeMathOverflow)?,
    )
    .map_err(|_| ErrorCode::FeeMathOverflow)?;
    Ok((
        base_credit,
        total_credit
            .checked_sub(base_credit)
            .ok_or(ErrorCode::FeeMathOverflow)?,
    ))
}
