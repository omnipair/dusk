use crate::transitions::lending::InternalLiquidationReceipt;
use crate::transitions::liquidity::{
    consume_hlp_tracking_unrealized_interest, prepare_concentrated_hlp_transition_at_current_state,
    rebase_hlp_tracking_for_socialized_loss,
};
use crate::transitions::{FeesReceipt, LeverageLifecycleTransition, LeverageSwapFeeCredit, LeverageSwapQuote};
use crate::{
    errors::ErrorCode,
    state::{ConcentratedCurveCache, Market, MarketAsset, ProtocolAuctionSplit},
    transitions::{
        liquidity::{prepare_concentrated_hlp_transition, ConcentratedHlpTransition, SwapCashPolicy},
        AmmSwapQuote, HlpRebalanceReceipt, HlpYieldEligibility, SwapFeeBreakdown,
    },
};
use anchor_lang::prelude::*;

/// All state-derived inputs frozen for one swap quote. Execution and preview
/// construct the same context after validating their instruction accounts and
/// reading `Clock` once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SwapRequest {
    pub current_slot: u64,
    pub current_unix_timestamp: i64,
    pub asset_in: MarketAsset,
    pub reserve_credit: u64,
    /// Protocol share frozen before the LP-owned remainder is split between
    /// claimable yield and native reserve compounding.
    pub protocol_fee_bps: u16,
}

/// State-only preparation shared by preview and execution. `finalize_state`
/// commits the matching state transition; token settlement remains an
/// instruction concern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedSwap {
    pub(crate) quoted_slot: u64,
    pub quote: AmmSwapQuote,
    pub base_pre_rebalance: HlpRebalanceReceipt,
    pub quote_pre_rebalance: HlpRebalanceReceipt,
    pub fee_eligible_ylp_supply: u64,
    pub interest_eligibility: HlpYieldEligibility,
    pub(crate) cash_policy: SwapCashPolicy,
    pub(crate) post_fee_curve_cache: Option<Box<ConcentratedCurveCache>>,
    pub(crate) concentrated_transition: Option<Box<ConcentratedHlpTransition>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FinalizedSwapState {
    pub base_rebalance: HlpRebalanceReceipt,
    pub quote_rebalance: HlpRebalanceReceipt,
    pub fees: FeesReceipt,
    pub lifecycle: LeverageLifecycleTransition,
    pub liquidation: Option<InternalLiquidationReceipt>,
}

/// Fixed-debt settlement composed with the AMM leg of a floor liquidation.
pub(crate) struct LendingSwapSettlement<'a> {
    pub position: &'a mut crate::state::BorrowPosition,
    pub debt_asset: MarketAsset,
    pub insurance_spent: u64,
    pub insurance_credit: u64,
    pub collateral_consumed: u64,
    pub caller_bounty: u64,
}

impl LendingSwapSettlement<'_> {
    fn apply(self, market: &mut Market, swap_output: u64, current_slot: u64) -> Result<InternalLiquidationReceipt> {
        market.settle_internal_liquidation(
            self.position,
            self.debt_asset,
            swap_output,
            self.insurance_spent,
            self.insurance_credit,
            self.collateral_consumed,
            self.caller_bounty,
            current_slot,
        )
    }
}

impl PreparedSwap {
    #[inline(always)]
    pub(crate) fn leverage_quote(&self) -> LeverageSwapQuote {
        LeverageSwapQuote::from_amm(self.quote, self.quoted_slot)
    }

    /// Executes the same complete market transition for spot and scratch previews.
    pub(crate) fn finalize_state(
        &mut self,
        market: &mut Market,
        current_slot: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<FinalizedSwapState> {
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(
            &self.leverage_quote(),
            self.quote.fee.claimable_fee_debit,
        )?;
        self.apply(
            market,
            SwapCashPolicy::Spot,
            credit,
            current_slot,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )
    }

    /// Owns every market mutation of one swap. The cash policy selects the debt
    /// lifecycle; callers cannot independently commit fees, hLP, or observations.
    /// Consuming the prepared hLP plan prevents a second application. Account
    /// validation, custody CPIs, and position-specific health checks stay outside.
    #[inline(never)]
    pub(crate) fn apply(
        &mut self,
        market: &mut Market,
        cash_policy: SwapCashPolicy,
        fee_credit: LeverageSwapFeeCredit,
        current_slot: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        lending: Option<LendingSwapSettlement<'_>>,
    ) -> Result<FinalizedSwapState> {
        require!(self.cash_policy == cash_policy, ErrorCode::BrokenInvariant);
        let quote = self.quote;
        let swap = self.leverage_quote();
        market.validate_leverage_swap_quote(swap, quote.asset_in, current_slot)?;
        require_eq!(quote.fee.protocol_fee_bps, protocol_fee_bps, ErrorCode::BrokenInvariant);
        require!(
            fee_credit == LeverageSwapFeeCredit::from_total_actual_credit(&swap, quote.fee.claimable_fee_debit,)?,
            ErrorCode::BrokenInvariant
        );
        if let Some(settlement) = &lending {
            require!(
                cash_policy
                    == SwapCashPolicy::Liquidate {
                        debt_asset: settlement.debt_asset,
                        debt_shares: 0,
                        debt_principal: 0,
                    },
                ErrorCode::BrokenInvariant
            );
        }
        let prepared_transition = self.concentrated_transition.take().ok_or(ErrorCode::BrokenInvariant)?;
        let prepared_cache = self.post_fee_curve_cache.take();
        let fee_asset = MarketAsset::try_from_code(quote.fee.fee_asset)?;
        let lifecycle = if cash_policy == SwapCashPolicy::Spot || lending.is_some() {
            let (side_in, side_out) = market.swap_sides_mut(quote.asset_in);
            require_gte!(
                side_out.reserves.cash_reserve,
                quote.gross_amount_out,
                ErrorCode::InsufficientLiquidity
            );
            side_in.credit_reserve(quote.fee.amount_in_for_quote, true)?;
            side_out.debit_reserve(quote.gross_amount_out, true)?;
            LeverageLifecycleTransition::default()
        } else {
            market.apply_leverage_lifecycle_transition(
                cash_policy,
                quote.asset_in,
                quote.fee.amount_in_for_quote,
                quote.amount_out,
                quote.gross_amount_out,
            )?
        };
        if quote.fee.compounded_fee_debit > 0 {
            market
                .side_mut(fee_asset)
                .credit_reserve(quote.fee.compounded_fee_debit, true)?;
        }
        // Ownership is frozen before the hLP share/debt reconstruction.
        let fees = market.side_mut(fee_asset).record_swap_fee_allocation(
            quote.fee.base_fee_debit,
            quote.fee.distributed_surcharge_debit,
            quote.fee.compounded_base_fee_debit,
            quote.fee.compounded_dynamic_surcharge_debit,
            protocol_fee_bps,
            protocol_auction_split,
            self.fee_eligible_ylp_supply,
        )?;
        market.base_side.assert_share_backing()?;
        market.quote_side.assert_share_backing()?;
        market.side(fee_asset).fees.assert_backed()?;
        let debt_asset = quote.asset_in.opposite();
        consume_hlp_tracking_unrealized_interest(
            &mut self.base_pre_rebalance,
            debt_asset,
            lifecycle.removed_unrealized_interest,
        )?;
        consume_hlp_tracking_unrealized_interest(
            &mut self.quote_pre_rebalance,
            debt_asset,
            lifecycle.removed_unrealized_interest,
        )?;
        let rebase = market.apply_leverage_socialized_loss(debt_asset, lifecycle, current_slot)?;
        let socialized_loss = lifecycle.socialized_principal_loss > 0;
        if socialized_loss {
            rebase_hlp_tracking_for_socialized_loss(&mut self.base_pre_rebalance, 0, rebase.base_nav_delta_nad)?;
            rebase_hlp_tracking_for_socialized_loss(&mut self.quote_pre_rebalance, 0, rebase.quote_nav_delta_nad)?;
        }
        if quote.fee.retained_surcharge > 0 {
            market.credit_protected_recenter_reserve(fee_asset, quote.fee.retained_surcharge)?;
        }
        let fresh_transition;
        let transition = if socialized_loss {
            fresh_transition = prepare_concentrated_hlp_transition_at_current_state(market)?;
            &fresh_transition
        } else {
            prepared_transition.as_ref()
        };
        let (base_rebalance, quote_rebalance) = transition.consume(market)?;
        if quote.fee.compounded_fee_debit > 0 {
            let cache = if socialized_loss {
                None
            } else {
                Some(*prepared_cache.ok_or(ErrorCode::BrokenInvariant)?)
            };
            market.checkpoint_amm_neutral_inventory_raw(current_slot, cache)?;
        } else {
            require!(prepared_cache.is_none(), ErrorCode::BrokenInvariant);
        }
        // Fixed-debt clearance can still change reserves after the AMM/hLP leg.
        // Observe the complete operation once, including that final change.
        let liquidation = lending
            .map(|settlement| settlement.apply(market, quote.amount_out, current_slot))
            .transpose()?;
        let final_price_nad = if let Some(receipt) = liquidation {
            if receipt.liquidation.socialized_loss > 0 {
                market.checkpoint_amm_socialized_loss_raw(current_slot)?.0
            } else {
                market.checkpoint_amm_neutral_inventory_raw(current_slot, None)?;
                market
                    .current_concentrated_spot_price_nad()?
                    .ok_or(ErrorCode::BrokenInvariant)?
            }
        } else if socialized_loss {
            market
                .current_concentrated_spot_price_nad()?
                .ok_or(ErrorCode::BrokenInvariant)?
        } else {
            quote.reserve_end_price_nad
        };
        require!(final_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        market.finalize_amm_trade_after_inventory_checkpoint(quote.start_price_nad, final_price_nad, current_slot)?;
        let depth = market
            .amm
            .concentrated_curve_cache
            .tail_liquidity
            .checked_add(market.amm.concentrated_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        market.observe_risk_from_concentrated_curve(final_price_nad, depth, current_slot)?;
        // Swaps preserve the lazy risk-refresh revision. A fixed-debt close
        // also completes the lending refresh, as its previous adapter did.
        if liquidation.is_some() {
            market.risk_revision = market.curve_revision;
        }
        market.assert_market_invariants()?;
        Ok(FinalizedSwapState {
            base_rebalance,
            quote_rebalance,
            fees,
            lifecycle,
            liquidation,
        })
    }
}

impl Market {
    /// Complete floor-liquidation state transition, also used by predictive execution.
    pub(crate) fn settle_backstop_swap(
        &mut self,
        prepared: Option<&mut PreparedSwap>,
        settlement: LendingSwapSettlement<'_>,
        current_slot: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<(Option<FinalizedSwapState>, InternalLiquidationReceipt)> {
        if let Some(prepared) = prepared {
            let policy = SwapCashPolicy::Liquidate {
                debt_asset: settlement.debt_asset,
                debt_shares: 0,
                debt_principal: 0,
            };
            let credit = LeverageSwapFeeCredit::from_total_actual_credit(
                &prepared.leverage_quote(),
                prepared.quote.fee.claimable_fee_debit,
            )?;
            let finalized = prepared.apply(
                self,
                policy,
                credit,
                current_slot,
                protocol_fee_bps,
                protocol_auction_split,
                Some(settlement),
            )?;
            let receipt = finalized.liquidation.ok_or(ErrorCode::BrokenInvariant)?;
            Ok((Some(finalized), receipt))
        } else {
            let receipt = settlement.apply(self, 0, current_slot)?;
            if receipt.liquidation.socialized_loss > 0 {
                self.finalize_amm_socialized_loss_and_observe_risk(current_slot)?;
            } else {
                self.finalize_amm_transition_and_observe_risk(current_slot)?;
            }
            Ok((None, receipt))
        }
    }
}

impl SwapRequest {
    pub(crate) fn prepare(self, market: &mut Market) -> Result<Box<PreparedSwap>> {
        self.prepare_with_cash_policy(market, SwapCashPolicy::Spot)
    }

    pub(crate) fn prepare_with_cash_policy(
        self,
        market: &mut Market,
        cash_policy: SwapCashPolicy,
    ) -> Result<Box<PreparedSwap>> {
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

        // Concentrated tail+band markets quote and hedge in one algebraic path.
        // Apply at most one center target derived from an earlier observation
        // before freezing this swap's fee/curve state. The observation made by
        // this swap can only schedule a target for a later operation.
        market.config.amm.concentrated_curve_parameters()?;
        market.advance_one_amm_controller_target(self.current_slot)?;
        let pre_state = market.dynamic_fee_pre_state(self.current_slot)?;
        let preliminary = market.preliminary_swap_inputs_for_state_at_time(
            self.asset_in,
            self.reserve_credit,
            self.current_slot,
            self.current_unix_timestamp,
            pre_state,
        )?;
        let integrated_start = market.integrated_curve_state_nad()?;
        let mut concentrated = market
            .quote_concentrated_integrated_with_fee_from_state(
                self.asset_in,
                self.reserve_credit,
                preliminary,
                integrated_start,
                self.protocol_fee_bps,
            )?
            .ok_or(ErrorCode::BrokenInvariant)?;
        if cash_policy == SwapCashPolicy::Spot {
            crate::transitions::liquidity::apply_concentrated_hlp_recovery(
                market,
                self.asset_in,
                integrated_start,
                &mut concentrated,
            )?;
        }
        let transition = prepare_concentrated_hlp_transition(market, concentrated, self.asset_in)?;
        require!(
            transition
                .interest_cash_floors(self.asset_in, concentrated.gross_amount_out)
                .available(market),
            ErrorCode::InsufficientLiquidity
        );
        let post_fee_curve_cache = concentrated.post_fee_curve_cache.map(Box::new);
        let quote = concentrated.as_swap_quote(self.asset_in);
        Ok(Box::new(PreparedSwap {
            quoted_slot: self.current_slot,
            quote,
            base_pre_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Base,
                ..HlpRebalanceReceipt::default()
            },
            quote_pre_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Quote,
                ..HlpRebalanceReceipt::default()
            },
            fee_eligible_ylp_supply: market
                .side(MarketAsset::try_from_code(quote.fee.fee_asset)?)
                .shares
                .ylp_supply,
            interest_eligibility,
            cash_policy,
            post_fee_curve_cache,
            concentrated_transition: Some(Box::new(transition)),
        }))
    }
}

pub(crate) fn split_claimable_fee_credit(fee: &SwapFeeBreakdown, total_credit: u64) -> Result<(u64, u64)> {
    let claimable_base_fee = fee
        .base_fee_debit
        .checked_sub(fee.compounded_base_fee_debit)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    let claimable_dynamic_surcharge = fee
        .distributed_surcharge_debit
        .checked_sub(fee.compounded_dynamic_surcharge_debit)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require_eq!(
        claimable_base_fee
            .checked_add(claimable_dynamic_surcharge)
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
            .checked_mul(claimable_base_fee as u128)
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
