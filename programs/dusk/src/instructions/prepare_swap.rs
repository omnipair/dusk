use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    market::{
        liquidity::{pre_solve_hlps_for_swap_joint, SwapCashPolicy},
        AmmSwapQuote, HlpRebalanceReceipt, SwapFeeBreakdown,
    },
    state::{HlpYieldEligibility, Market, MarketAsset, ProtocolAuctionSplit},
};

/// All state-derived inputs frozen for one swap quote. Execution and preview
/// construct the same context after validating their instruction accounts and
/// reading `Clock` once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SwapRequest {
    pub current_slot: u64,
    pub asset_in: MarketAsset,
    pub reserve_credit: u64,
}

/// State-only preparation shared by preview and execution. `finalize_state`
/// commits the matching state transition; token settlement remains an
/// instruction concern.
#[derive(Debug)]
pub(crate) struct PreparedSwap {
    pub quote: AmmSwapQuote,
    pub base_pre_rebalance: HlpRebalanceReceipt,
    pub quote_pre_rebalance: HlpRebalanceReceipt,
    pub fee_eligible_ylp_supply: u64,
    pub interest_eligibility: HlpYieldEligibility,
    pub cash_policy: SwapCashPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FinalizedSwapState {
    pub base_rebalance: HlpRebalanceReceipt,
    pub quote_rebalance: HlpRebalanceReceipt,
}

impl PreparedSwap {
    /// Commits every state-only consequence of an already prepared swap.
    /// Spot execution and preview share this path; token transfers, yLP mint/
    /// burn CPIs, and reserve-custody checks remain instruction concerns.
    pub(crate) fn finalize_state(
        &self,
        market: &mut Market,
        current_slot: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<FinalizedSwapState> {
        require!(self.cash_policy == SwapCashPolicy::Spot, ErrorCode::BrokenInvariant);
        let quote = self.quote;
        let (base_fee_credit, distributed_surcharge_credit) =
            split_claimable_fee_credit(&quote.fee, quote.fee.claimable_fee_debit)?;
        let trade_endpoint = quote.trade_endpoint()?;
        let reserve_endpoint = quote.reserve_endpoint()?;

        require_eq!(
            quote.fee.reserve_input_credit,
            quote
                .fee
                .amount_in_for_quote
                .checked_add(quote.fee.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        {
            let (side_in, side_out) = market.swap_sides_mut(quote.asset_in);
            require_gte!(
                side_out.reserves.cash_reserve,
                quote.amount_out,
                ErrorCode::InsufficientLiquidity
            );
            side_in.credit_reserve(quote.fee.amount_in_for_quote, true)?;
            side_out.debit_reserve(quote.amount_out, true)?;
        }

        // Reuse the identity-bound quote endpoints. The invariant-preserving
        // trade is neutral; only retained surcharge funds protected principal.
        market.ensure_amm_initialized(current_slot)?;
        require!(market.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = trade_endpoint.validated_evaluation(market, current_slot)?;
        let q_per_share_nad = market.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        market.amm.commit_invariant(evaluation.invariant_d)?;
        market.amm.checkpoint_neutral_liquidity(q_per_share_nad);

        if quote.fee.retained_surcharge > 0 {
            market
                .side_mut(quote.asset_in)
                .credit_reserve(quote.fee.retained_surcharge, true)?;
            let evaluation = reserve_endpoint.validated_evaluation(market, current_slot)?;
            let q_per_share_nad = market.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
            market.amm.commit_invariant(evaluation.invariant_d)?;
            market.amm.checkpoint_retained_surcharge(q_per_share_nad)?;
        }

        {
            let (side_in, side_out) = market.swap_sides_mut(quote.asset_in);
            side_in.record_claimable_swap_fees(
                base_fee_credit,
                distributed_surcharge_credit,
                protocol_fee_bps,
                protocol_auction_split,
                self.fee_eligible_ylp_supply,
            )?;
            side_in.assert_share_backing()?;
            side_out.assert_share_backing()?;
            side_in.fees.assert_backed()?;
        }

        market.finalize_amm_trade_after_inventory_checkpoint(
            quote.start_price_nad,
            quote.end_price_nad,
            current_slot,
        )?;
        let (base_rebalance, quote_rebalance, concentrated_curve_evaluation) = market.finalize_hlp_vaults_for_swap(
            self.base_pre_rebalance,
            self.quote_pre_rebalance,
            current_slot,
            Some(quote.reserve_end_price_nad),
        )?;
        let h_lp_tokens_will_change =
            rebalance_executes_token_changes(&base_rebalance) || rebalance_executes_token_changes(&quote_rebalance);
        let h_lp_mutates_curve_inventory = hlp_receipt_mutates_curve_inventory(&base_rebalance)
            || hlp_receipt_mutates_curve_inventory(&quote_rebalance);
        require!(
            !h_lp_tokens_will_change || h_lp_mutates_curve_inventory,
            ErrorCode::BrokenInvariant
        );

        let final_curve_evaluation = if let Some(evaluation) = concentrated_curve_evaluation {
            require!(h_lp_mutates_curve_inventory, ErrorCode::BrokenInvariant);
            evaluation
        } else if h_lp_mutates_curve_inventory {
            market.checkpoint_amm_neutral_inventory(current_slot)?
        } else if quote.fee.retained_surcharge > 0 {
            reserve_endpoint.evaluation()
        } else {
            trade_endpoint.evaluation()
        };
        market.observe_risk_from_curve_evaluation(final_curve_evaluation, current_slot)?;

        Ok(FinalizedSwapState {
            base_rebalance,
            quote_rebalance,
        })
    }
}

impl SwapRequest {
    pub(crate) fn prepare(self, market: &mut Market) -> Result<PreparedSwap> {
        self.prepare_with_cash_policy(market, SwapCashPolicy::Spot)
    }

    pub(crate) fn prepare_with_cash_policy(
        self,
        market: &mut Market,
        cash_policy: SwapCashPolicy,
    ) -> Result<PreparedSwap> {
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

        let (base_pre_rebalance, quote_pre_rebalance, fee_eligible_ylp_supply, concentrated_quote) = if has_active_hlp {
            require_gte!(
                preliminary.reserve_input_credit,
                preliminary.amount_in_for_quote,
                ErrorCode::BrokenInvariant
            );
            let (base, quote, swap_quote) = pre_solve_hlps_for_swap_joint(
                market,
                self.asset_in,
                self.reserve_credit,
                self.current_slot,
                pre_state,
                preliminary,
                cash_policy,
            )?;
            let concentrated_quote = Some(swap_quote);
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
            (base, quote, eligible_supply, concentrated_quote)
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
                None,
            )
        };

        let concentrated_retention = concentrated_quote.map(|_| market.amm.retain_dynamic_surcharge);
        if hlp_receipt_mutates_curve_inventory(&base_pre_rebalance)
            || hlp_receipt_mutates_curve_inventory(&quote_pre_rebalance)
        {
            if concentrated_quote.is_none() {
                market.checkpoint_amm_neutral_inventory(self.current_slot)?;
            }
        } else {
            market.ensure_amm_initialized(self.current_slot)?;
        }
        if let Some(concentrated_retention) = concentrated_retention {
            require_eq!(
                market.amm.retain_dynamic_surcharge,
                concentrated_retention,
                ErrorCode::BrokenInvariant
            );
        }
        let quote = if let Some(quote) = concentrated_quote {
            quote
        } else {
            market.quote_amm_swap_for_reserves_nad(
                self.asset_in,
                self.reserve_credit,
                self.current_slot,
                market.curve_reserves_nad()?,
                pre_state,
                preliminary,
            )?
        };
        require!(
            cash_policy
                .floors(market, self.asset_in, quote.amount_out)?
                .available(market),
            ErrorCode::InsufficientLiquidity
        );
        if let Some(operation_start_price_nad) = operation_start_price_nad {
            market.require_residual_hlp_swap_safety(
                operation_start_price_nad as u128,
                quote.reserve_end_price_nad as u128,
                base_hlp_residual_on_entry,
                quote_hlp_residual_on_entry,
            )?;
        }

        Ok(PreparedSwap {
            quote,
            base_pre_rebalance,
            quote_pre_rebalance,
            fee_eligible_ylp_supply,
            interest_eligibility,
            cash_policy,
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

pub(crate) fn rebalance_executes_token_changes(receipt: &HlpRebalanceReceipt) -> bool {
    receipt.ylp_mint_amount > 0 || receipt.ylp_burn_amount > 0 || receipt.interest_paid > 0
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
