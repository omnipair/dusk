use anchor_lang::prelude::*;

use super::{
    AmmSwapQuote, FeesReceipt, HlpRebalanceReceipt, HlpYieldEligibility, Market, MarketAsset, SwapFeeBreakdown,
};
use crate::state::ProtocolAuctionSplit;
use crate::{
    constants::{
        BPS_DENOMINATOR, LEVERAGE_INITIAL_MARGIN_BPS, LEVERAGE_MAINTENANCE_BUFFER_BPS, LEVERAGE_MAX_MULTIPLIER_BPS,
        LEVERAGE_MAX_UNWIND_IMPACT_BPS, LIQUIDATION_INCENTIVE_BPS,
    },
    errors::ErrorCode,
    math::{denormalize_from_nad_floor, normalize_to_nad, DynamicFeePreState},
    state::LeveragePosition,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageSwapQuote {
    pub asset_in: u8,
    pub quoted_slot: u64,
    pub amount_in: u64,
    pub amount_in_after_fee: u64,
    pub reserve_input_credit: u64,
    pub amount_out: u64,
    pub start_price_nad: u64,
    /// Invariant-preserving trade endpoint; retained principal is excluded.
    pub end_price_nad: u64,
    /// Final executable-reserve marginal price after retained principal.
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    /// Nominal claimable fee debit held in the reserve vault but excluded from
    /// executable reserves. Actual Token-2022 credit is recorded separately
    /// through `LeverageSwapFeeCredit`.
    pub fee_credit: u64,
    pub fee_breakdown: SwapFeeBreakdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageSwapFeeCredit {
    pub base: u64,
    pub distributed_surcharge: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeverageSwapPlan {
    pub swap: LeverageSwapQuote,
    pub base_pre_rebalance: HlpRebalanceReceipt,
    pub quote_pre_rebalance: HlpRebalanceReceipt,
    pub fee_eligible_ylp_supply: u64,
    pub interest_eligibility: HlpYieldEligibility,
}

impl LeverageSwapQuote {
    pub(crate) fn from_amm(quote: AmmSwapQuote, current_slot: u64) -> Self {
        Self {
            asset_in: quote.asset_in.code(),
            quoted_slot: current_slot,
            amount_in: quote.fee.reserve_credit,
            amount_in_after_fee: quote.fee.amount_in_for_quote,
            reserve_input_credit: quote.fee.reserve_input_credit,
            amount_out: quote.amount_out,
            start_price_nad: quote.start_price_nad,
            end_price_nad: quote.end_price_nad,
            reserve_end_price_nad: quote.reserve_end_price_nad,
            decayed_volatility_nad: quote.decayed_volatility_nad,
            post_success_volatility_nad: quote.post_success_volatility_nad,
            fee_credit: quote.fee.claimable_fee_debit,
            fee_breakdown: quote.fee,
        }
    }
}

impl LeverageSwapFeeCredit {
    pub fn from_total_actual_credit(quote: &LeverageSwapQuote, total_credit: u64) -> Result<Self> {
        let fee = quote.fee_breakdown;
        require_gte!(fee.claimable_fee_debit, total_credit, ErrorCode::BrokenInvariant);
        if fee.claimable_fee_debit == 0 {
            require_eq!(total_credit, 0, ErrorCode::BrokenInvariant);
            return Ok(Self::default());
        }
        let base = u64::try_from(
            (total_credit as u128)
                .checked_mul(fee.base_fee_debit as u128)
                .and_then(|value| value.checked_div(fee.claimable_fee_debit as u128))
                .ok_or(ErrorCode::FeeMathOverflow)?,
        )
        .map_err(|_| ErrorCode::FeeMathOverflow)?;
        Ok(Self {
            base,
            distributed_surcharge: total_credit.checked_sub(base).ok_or(ErrorCode::FeeMathOverflow)?,
        })
    }

    fn validate_for_quote(self, quote: &LeverageSwapQuote) -> Result<()> {
        let total = self
            .base
            .checked_add(self.distributed_surcharge)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(
            self == Self::from_total_actual_credit(quote, total)?,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageOpenReceipt {
    pub borrowed_amount: u64,
    pub debt_amount: u64,
    pub debt_shares: u128,
    pub notional: u64,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub equity: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageUpdateReceipt {
    pub borrowed_amount: u64,
    pub debt_delta: i64,
    pub collateral_delta: i64,
    pub debt_amount: u64,
    pub debt_shares: u128,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub interest_paid: u64,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageCloseReceipt {
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub residual: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageLiquidationReceipt {
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub principal_written_off: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub liquidator_amount: u64,
    pub owner_residual: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
}

impl Market {
    pub fn quote_leverage_swap(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
    ) -> Result<LeverageSwapQuote> {
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        let preliminary = self.preliminary_swap_inputs_for_state(amount_in, current_slot, pre_state)?;
        let quote = self.quote_amm_swap_for_reserves_nad(
            asset_in,
            amount_in,
            current_slot,
            self.curve_reserves_nad()?,
            pre_state,
            preliminary,
        )?;
        Ok(LeverageSwapQuote::from_amm(quote, current_slot))
    }

    fn validate_leverage_swap_quote(
        &self,
        quote: LeverageSwapQuote,
        asset_in: MarketAsset,
        current_slot: u64,
    ) -> Result<()> {
        let fee = quote.fee_breakdown;
        require!(quote.asset_in == asset_in.code(), ErrorCode::BrokenInvariant);
        require_eq!(quote.quoted_slot, current_slot, ErrorCode::BrokenInvariant);
        require!(quote.amount_in > 0 && quote.amount_out > 0, ErrorCode::BrokenInvariant);
        require_eq!(fee.reserve_credit, quote.amount_in, ErrorCode::BrokenInvariant);
        require_eq!(
            fee.amount_in_for_quote,
            quote.amount_in_after_fee,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.reserve_input_credit,
            quote.reserve_input_credit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(fee.claimable_fee_debit, quote.fee_credit, ErrorCode::BrokenInvariant);
        require_eq!(
            fee.base_fee_debit
                .checked_add(fee.dynamic_surcharge_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.total_fee_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.retained_surcharge
                .checked_add(fee.distributed_surcharge_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.dynamic_surcharge_debit,
            ErrorCode::BrokenInvariant
        );
        if self.amm.retain_dynamic_surcharge {
            require_eq!(
                fee.retained_surcharge,
                fee.dynamic_surcharge_debit,
                ErrorCode::BrokenInvariant
            );
            require_eq!(fee.distributed_surcharge_debit, 0, ErrorCode::BrokenInvariant);
        } else {
            require_eq!(fee.retained_surcharge, 0, ErrorCode::BrokenInvariant);
            require_eq!(
                fee.distributed_surcharge_debit,
                fee.dynamic_surcharge_debit,
                ErrorCode::BrokenInvariant
            );
        }
        require_eq!(
            fee.amount_in_for_quote
                .checked_add(fee.total_fee_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.reserve_credit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.reserve_input_credit
                .checked_add(fee.claimable_fee_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.reserve_credit,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }

    #[cfg(test)]
    fn leverage_amm_quote(quote: LeverageSwapQuote, asset_in: MarketAsset) -> AmmSwapQuote {
        AmmSwapQuote::new_without_endpoints(
            asset_in,
            quote.amount_out,
            quote.start_price_nad,
            quote.end_price_nad,
            quote.reserve_end_price_nad,
            quote.decayed_volatility_nad,
            quote.post_success_volatility_nad,
            quote.fee_breakdown,
        )
    }

    /// Commits the AMM leg, performs the same exact inline hLP correction used
    /// by spot, then materializes the final curve/risk identity. The returned
    /// receipts are settled by the instruction as one net token change per
    /// side; no maintenance call can be required later.
    fn finalize_leverage_swap_hlp(
        &mut self,
        plan: LeverageSwapPlan,
        current_slot: u64,
    ) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
        self.finalize_amm_trade_after_inventory_checkpoint(
            plan.swap.start_price_nad,
            plan.swap.end_price_nad,
            current_slot,
        )?;
        let receipts =
            self.finalize_hlp_vaults_for_swap(plan.base_pre_rebalance, plan.quote_pre_rebalance, current_slot)?;
        let final_curve_evaluation = self.checkpoint_amm_neutral_inventory(current_slot)?;
        // The leverage quote was frozen before reserve, debt, retained-fee,
        // and hLP mutations. Record the actual final executable endpoint so a
        // later risk-sensitive operation integrates this post-trade mark over
        // elapsed time rather than extending the stale pre-trade observation.
        // The checkpoint above already performed the only final root solve.
        self.observe_risk_from_curve_evaluation(final_curve_evaluation, current_slot)?;
        Ok(receipts)
    }

    pub fn open_leverage(
        &mut self,
        position: &mut LeveragePosition,
        owner: Pubkey,
        market: Pubkey,
        position_id: Pubkey,
        referral_partner: Pubkey,
        referral_interest_share_bps: u16,
        debt_asset: MarketAsset,
        margin_credit: u64,
        multiplier_bps: u64,
        collateral_credit: u64,
        plan: LeverageSwapPlan,
        swap_fee_credit: LeverageSwapFeeCredit,
        opened_at: i64,
        opened_slot: u64,
        bump: u8,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<LeverageOpenReceipt> {
        let swap = plan.swap;
        require!(margin_credit > 0, ErrorCode::AmountZero);
        require!(multiplier_bps > BPS_DENOMINATOR as u64, ErrorCode::InvalidArgument);
        require!(
            multiplier_bps <= LEVERAGE_MAX_MULTIPLIER_BPS,
            ErrorCode::LeverageMultiplierTooHigh
        );
        let borrowed_amount = leverage_debt_from_margin(margin_credit, multiplier_bps)?;
        let notional = margin_credit
            .checked_add(borrowed_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.ensure_amm_initialized(opened_slot)?;
        require_eq!(swap.amount_in, notional, ErrorCode::BrokenInvariant);
        self.validate_leverage_swap_quote(swap, debt_asset, opened_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);

        let closeout_quote = self.post_swap_closeout_quote_with_quote(
            debt_asset,
            swap,
            debt_asset.opposite(),
            collateral_credit,
            opened_slot,
        )?;
        let pre_finalize_closeout_value = closeout_quote.amount_out;
        let post_swap_spot_price = closeout_quote.start_price_nad;
        require_initial_leverage_health(
            self,
            debt_asset.opposite(),
            collateral_credit,
            post_swap_spot_price,
            pre_finalize_closeout_value,
            borrowed_amount,
        )?;
        self.record_leverage_borrow(debt_asset, borrowed_amount, opened_slot)?;
        let fees = self.apply_leverage_swap(
            debt_asset,
            swap,
            swap.amount_out,
            0,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            plan.fee_eligible_ylp_supply,
            opened_slot,
        )?;
        let debt_shares = self.add_isolated_borrow_debt(debt_asset, borrowed_amount)?;
        position.initialize(
            owner,
            market,
            position_id,
            referral_partner,
            referral_interest_share_bps,
            debt_asset,
            collateral_credit,
            margin_credit,
            notional,
            borrowed_amount,
            debt_shares,
            multiplier_bps,
            opened_at,
            opened_slot,
            bump,
        );
        let (base_hlp_rebalance, quote_hlp_rebalance) = self.finalize_leverage_swap_hlp(plan, opened_slot)?;
        let closeout_value = self.require_position_initial_leverage_health(position, opened_slot)?;
        let equity = closeout_value
            .checked_sub(borrowed_amount)
            .ok_or(ErrorCode::LeverageInitialMarginTooLow)?;
        Ok(LeverageOpenReceipt {
            borrowed_amount,
            debt_amount: borrowed_amount,
            debt_shares,
            notional,
            collateral_amount: collateral_credit,
            closeout_value,
            equity,
            swap,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn increase_leverage(
        &mut self,
        position: &mut LeveragePosition,
        borrowed_amount: u64,
        collateral_credit: u64,
        plan: LeverageSwapPlan,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        let swap = plan.swap;
        position.require_open()?;
        require!(borrowed_amount > 0, ErrorCode::AmountZero);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        self.ensure_amm_initialized(current_slot)?;
        require_eq!(swap.amount_in, borrowed_amount, ErrorCode::BrokenInvariant);
        self.validate_leverage_swap_quote(swap, debt_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        let collateral_after = position
            .collateral_amount
            .checked_add(collateral_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let debt_after = debt_before
            .checked_add(borrowed_amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let closeout_quote = self.post_swap_closeout_quote_with_quote(
            debt_asset,
            swap,
            debt_asset.opposite(),
            collateral_after,
            current_slot,
        )?;
        let pre_finalize_closeout_value = closeout_quote.amount_out;
        let post_swap_spot_price = closeout_quote.start_price_nad;
        require_initial_leverage_health(
            self,
            debt_asset.opposite(),
            collateral_after,
            post_swap_spot_price,
            pre_finalize_closeout_value,
            debt_after,
        )?;
        self.record_leverage_borrow(debt_asset, borrowed_amount, current_slot)?;
        let fees = self.apply_leverage_swap(
            debt_asset,
            swap,
            swap.amount_out,
            0,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            plan.fee_eligible_ylp_supply,
            current_slot,
        )?;
        let added_shares = self.add_isolated_borrow_debt(debt_asset, borrowed_amount)?;
        position.debt_shares = position
            .debt_shares
            .checked_add(added_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(borrowed_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        position.credit_collateral(collateral_credit)?;
        let (base_hlp_rebalance, quote_hlp_rebalance) = self.finalize_leverage_swap_hlp(plan, current_slot)?;
        let closeout_value = self.require_position_initial_leverage_health(position, current_slot)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount,
            debt_delta: i64::try_from(borrowed_amount).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: i64::try_from(collateral_credit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: position.debt_amount(&self.debt)?,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn decrease_leverage(
        &mut self,
        position: &mut LeveragePosition,
        collateral_debit: u64,
        min_repay_out: u64,
        plan: LeverageSwapPlan,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        let swap = plan.swap;
        position.require_open()?;
        require!(collateral_debit > 0, ErrorCode::AmountZero);
        require_gt!(
            position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_before = position.debt_amount(&self.debt)?;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_debit,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, min_repay_out, ErrorCode::SlippageExceeded);
        require_gt!(debt_before, swap.amount_out, ErrorCode::InsufficientDebt);
        let repayment = self
            .debt
            .isolated_repayment_for_max(debt_asset, position.debt_shares, swap.amount_out)?;
        // This instruction has no debt-token refund account. Reject a quote in
        // a share-granularity gap instead of silently donating the unused output.
        require_eq!(
            repayment.cash_repaid,
            swap.amount_out,
            ErrorCode::DebtShareDivisionOverflow
        );
        let collateral_after = position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let debt_after = debt_before
            .checked_sub(swap.amount_out)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let pre_finalize_closeout_value = self
            .post_swap_closeout_quote_with_quote(
                collateral_asset,
                swap,
                collateral_asset,
                collateral_after,
                current_slot,
            )?
            .amount_out;
        require_leverage_not_liquidatable(pre_finalize_closeout_value, debt_after)?;
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            swap.amount_out,
        )?;
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            clearance.interest_paid,
            live_debit,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            plan.fee_eligible_ylp_supply,
            current_slot,
        )?;
        position.debit_collateral(collateral_debit)?;
        let (base_hlp_rebalance, quote_hlp_rebalance) = self.finalize_leverage_swap_hlp(plan, current_slot)?;
        let closeout_value = self.leverage_closeout_value(position, current_slot)?;
        require_leverage_not_liquidatable(closeout_value, clearance.remaining_debt)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: 0,
            debt_delta: -i64::try_from(clearance.debt_reduced).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: -i64::try_from(collateral_debit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: clearance.remaining_debt,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: clearance.interest_paid,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn close_leverage(
        &mut self,
        position: &mut LeveragePosition,
        min_residual_out: u64,
        plan: LeverageSwapPlan,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageCloseReceipt> {
        let swap = plan.swap;
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let displayed_debt = position.debt_amount(&self.debt)?;
        require_gt!(displayed_debt, 0, ErrorCode::ZeroDebtAmount);
        let debt_amount = self
            .debt
            .isolated_repayment_for_max(debt_asset, position.debt_shares, u64::MAX)?
            .cash_repaid;
        let collateral_sold = position.collateral_amount;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, debt_amount, ErrorCode::InsufficientAmount);
        let residual = swap
            .amount_out
            .checked_sub(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(residual, min_residual_out, ErrorCode::SlippageExceeded);
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            debt_amount,
        )?;
        require_eq!(clearance.cash_repaid, debt_amount, ErrorCode::BrokenInvariant);
        require_eq!(clearance.remaining_debt, 0, ErrorCode::BrokenInvariant);
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let cash_debit = residual
            .checked_add(clearance.interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            cash_debit,
            live_debit,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            plan.fee_eligible_ylp_supply,
            current_slot,
        )?;
        position.collateral_amount = 0;
        let (base_hlp_rebalance, quote_hlp_rebalance) = self.finalize_leverage_swap_hlp(plan, current_slot)?;
        Ok(LeverageCloseReceipt {
            debt_repaid: clearance.cash_repaid,
            interest_paid: clearance.interest_paid,
            collateral_sold,
            closeout_value: swap.amount_out,
            residual,
            swap,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn liquidate_leverage(
        &mut self,
        position: &mut LeveragePosition,
        plan: LeverageSwapPlan,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageLiquidationReceipt> {
        let swap = plan.swap;
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let collateral_sold = position.collateral_amount;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        let margin_bps = equity_bps(swap.amount_out, debt_amount)?;
        require!(
            swap.amount_out <= debt_amount || margin_bps <= LEVERAGE_MAINTENANCE_BUFFER_BPS as u128,
            ErrorCode::LeveragePositionNotLiquidatable
        );

        let full_repayment = self
            .debt
            .isolated_repayment_for_max(debt_asset, position.debt_shares, u64::MAX)?;
        let repay_credit = swap.amount_out.min(full_repayment.cash_repaid);
        let aggregate_shares = match debt_asset {
            MarketAsset::Base => &mut self.debt.isolated_base_shares,
            MarketAsset::Quote => &mut self.debt.isolated_quote_shares,
        };
        require_gte!(
            *aggregate_shares,
            position.debt_shares,
            ErrorCode::DebtShareMathOverflow
        );
        let aggregate_principal = match debt_asset {
            MarketAsset::Base => &mut self.debt.isolated_base_principal,
            MarketAsset::Quote => &mut self.debt.isolated_quote_principal,
        };
        let position_principal_u64 = u64::try_from(position.debt_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
        require_gte!(
            *aggregate_principal,
            position_principal_u64,
            ErrorCode::DebtMathOverflow
        );
        let position_principal = position.debt_principal;
        let repayment_basis = (full_repayment.cash_repaid as u128).max(position_principal);
        let (principal_paid, interest_paid) =
            crate::math::realized_interest_split(repay_credit, repayment_basis, position_principal)?;
        let clearance = super::DebtClearance {
            shares_burned: position.debt_shares,
            cash_repaid: repay_credit,
            debt_reduced: full_repayment.position_debt_reduced,
            aggregate_debt_reduced: repay_credit,
            principal_paid,
            interest_paid,
            remaining_debt: 0,
        };
        let writeoff = super::DebtWriteoff {
            shares_written_off: 0,
            debt_written_off: full_repayment.position_debt_reduced.saturating_sub(repay_credit),
            aggregate_debt_written_off: full_repayment
                .cash_repaid
                .checked_sub(repay_credit)
                .ok_or(ErrorCode::DebtMathOverflow)?,
            principal_written_off: position_principal_u64.saturating_sub(principal_paid),
        };
        *aggregate_shares = aggregate_shares
            .checked_sub(position.debt_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *aggregate_principal = aggregate_principal
            .checked_sub(position_principal_u64)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        position.debt_shares = 0;
        position.debt_principal = 0;
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let residual = swap.amount_out.saturating_sub(full_repayment.cash_repaid);
        let max_incentive = (debt_amount as u128)
            .checked_mul(LIQUIDATION_INCENTIVE_BPS as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)? as u64;
        let liquidator_amount = residual.min(max_incentive);
        let owner_residual = residual
            .checked_sub(liquidator_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let cash_debit = residual
            .checked_add(clearance.interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            cash_debit,
            live_debit,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            plan.fee_eligible_ylp_supply,
            current_slot,
        )?;
        if writeoff.aggregate_debt_written_off > 0 {
            let debt_side = self.side_mut(debt_asset);
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_sub(writeoff.aggregate_debt_written_off)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            debt_side.assert_share_backing()?;
        }
        if writeoff.principal_written_off > 0 {
            self.checkpoint_amm_socialized_loss_raw(current_slot)?;
        }
        position.collateral_amount = 0;
        let (base_hlp_rebalance, quote_hlp_rebalance) = self.finalize_leverage_swap_hlp(plan, current_slot)?;
        Ok(LeverageLiquidationReceipt {
            debt_repaid: clearance.cash_repaid,
            interest_paid: clearance.interest_paid,
            principal_written_off: writeoff.principal_written_off,
            collateral_sold,
            closeout_value: swap.amount_out,
            liquidator_amount,
            owner_residual,
            swap,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn add_leverage_margin(
        &mut self,
        position: &mut LeveragePosition,
        repay_credit: u64,
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(repay_credit > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        require_gt!(debt_before, repay_credit, ErrorCode::InsufficientDebt);
        let pre_finalize_closeout_value = self.leverage_closeout_value(position, current_slot)?;
        let debt_after = debt_before
            .checked_sub(repay_credit)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        require_leverage_not_liquidatable(pre_finalize_closeout_value, debt_after)?;
        let repayment = self
            .debt
            .isolated_repayment_for_max(debt_asset, position.debt_shares, repay_credit)?;
        require_eq!(repayment.cash_repaid, repay_credit, ErrorCode::BrokenInvariant);
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            repay_credit,
        )?;
        let principal_paid = clearance.principal_paid;
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let side = self.side_mut(debt_asset);
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_sub(live_debit)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        side.reserves.cash_reserve = side
            .reserves
            .cash_reserve
            .checked_add(principal_paid)
            .ok_or(ErrorCode::ReserveOverflow)?;
        self.finalize_amm_transition_and_observe_risk(current_slot)?;
        // Adding margin only reduces debt, so it remains available as a rescue
        // path even when the position's final-curve health is poor.
        let closeout_value = self.leverage_closeout_value(position, current_slot)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: 0,
            debt_delta: -i64::try_from(clearance.debt_reduced).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: 0,
            debt_amount: clearance.remaining_debt,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: clearance.interest_paid,
            fees: FeesReceipt::default(),
            base_hlp_rebalance: HlpRebalanceReceipt::default(),
            quote_hlp_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Quote,
                ..HlpRebalanceReceipt::default()
            },
        })
    }

    pub fn remove_leverage_margin(
        &mut self,
        position: &mut LeveragePosition,
        borrow_amount: u64,
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        let debt_after = debt_before
            .checked_add(borrow_amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let collateral_asset = position.collateral_asset()?;
        let pre_finalize_closeout_quote =
            self.quote_leverage_swap(collateral_asset, position.collateral_amount, current_slot)?;
        require_initial_leverage_health(
            self,
            collateral_asset,
            position.collateral_amount,
            pre_finalize_closeout_quote.start_price_nad,
            pre_finalize_closeout_quote.amount_out,
            debt_after,
        )?;
        self.record_leverage_borrow(debt_asset, borrow_amount, current_slot)?;
        let shares = self.add_isolated_borrow_debt(debt_asset, borrow_amount)?;
        position.debt_shares = position
            .debt_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(borrow_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        self.finalize_amm_transition_and_observe_risk(current_slot)?;
        let closeout_value = self.require_position_initial_leverage_health(position, current_slot)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: borrow_amount,
            debt_delta: i64::try_from(borrow_amount).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: 0,
            debt_amount: position.debt_amount(&self.debt)?,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
            fees: FeesReceipt::default(),
            base_hlp_rebalance: HlpRebalanceReceipt::default(),
            quote_hlp_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Quote,
                ..HlpRebalanceReceipt::default()
            },
        })
    }

    pub fn leverage_closeout_value(&self, position: &LeveragePosition, current_slot: u64) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        self.quote_leverage_swap(collateral_asset, position.collateral_amount, current_slot)
            .map(|quote| quote.amount_out)
    }

    fn require_position_initial_leverage_health(&self, position: &LeveragePosition, current_slot: u64) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        let closeout_quote = self.quote_leverage_swap(collateral_asset, position.collateral_amount, current_slot)?;
        let closeout_value = closeout_quote.amount_out;
        let spot_price_nad = closeout_quote.start_price_nad;
        require_initial_leverage_health(
            self,
            collateral_asset,
            position.collateral_amount,
            spot_price_nad,
            closeout_value,
            position.debt_amount(&self.debt)?,
        )?;
        Ok(closeout_value)
    }

    fn post_swap_closeout_quote_with_quote(
        &self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        current_slot: u64,
    ) -> Result<AmmSwapQuote> {
        require_eq!(
            swap.fee_breakdown.reserve_input_credit,
            swap.fee_breakdown
                .amount_in_for_quote
                .checked_add(swap.fee_breakdown.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        let mut reserves = self.curve_reserves_nad()?;
        let input_nad = normalize_to_nad(
            swap.fee_breakdown.reserve_input_credit as u128,
            self.side(asset_in).asset_decimals,
        )?;
        let output_nad = normalize_to_nad(swap.amount_out as u128, self.side(asset_in.opposite()).asset_decimals)?;
        match asset_in {
            MarketAsset::Base => {
                reserves.base = reserves.base.checked_add(input_nad).ok_or(ErrorCode::ReserveOverflow)?;
                reserves.quote = reserves
                    .quote
                    .checked_sub(output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
            }
            MarketAsset::Quote => {
                reserves.quote = reserves
                    .quote
                    .checked_add(input_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                reserves.base = reserves
                    .base
                    .checked_sub(output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
            }
        }
        let pre_state = DynamicFeePreState {
            center_price_nad: self.current_curve_center_price_nad()?,
            volatility_accumulator_nad: swap.post_success_volatility_nad,
            volatility_last_update_slot: current_slot,
        };
        let preliminary = self.preliminary_swap_inputs_for_state(collateral_amount, current_slot, pre_state)?;
        self.quote_amm_swap_for_reserves_nad(
            collateral_asset,
            collateral_amount,
            current_slot,
            reserves,
            pre_state,
            preliminary,
        )
    }

    fn apply_leverage_swap(
        &mut self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        cash_debit_out: u64,
        extra_live_debit_out: u64,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        fee_eligible_ylp_supply: u64,
        current_slot: u64,
    ) -> Result<FeesReceipt> {
        swap_fee_credit.validate_for_quote(&swap)?;
        require_eq!(
            swap.reserve_input_credit,
            swap.amount_in_after_fee
                .checked_add(swap.fee_breakdown.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        {
            let (side_in, side_out) = self.swap_sides_mut(asset_in);
            side_in.reserves.live_reserve = side_in
                .reserves
                .live_reserve
                .checked_add(swap.amount_in_after_fee)
                .ok_or(ErrorCode::ReserveOverflow)?;
            side_in.reserves.cash_reserve = side_in
                .reserves
                .cash_reserve
                .checked_add(swap.amount_in_after_fee)
                .ok_or(ErrorCode::ReserveOverflow)?;
            side_out.reserves.live_reserve = side_out
                .reserves
                .live_reserve
                .checked_sub(
                    swap.amount_out
                        .checked_add(extra_live_debit_out)
                        .ok_or(ErrorCode::ReserveUnderflow)?,
                )
                .ok_or(ErrorCode::ReserveUnderflow)?;
            side_out.reserves.cash_reserve = side_out
                .reserves
                .cash_reserve
                .checked_sub(cash_debit_out)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
        }

        // The actual curve trade plus debt-accounting offsets is neutral to
        // protected liquidity. Only the retained surcharge that follows may
        // increase the recenter budget.
        self.checkpoint_amm_neutral_inventory_raw(current_slot)?;
        if swap.fee_breakdown.retained_surcharge > 0 {
            let retained = swap.fee_breakdown.retained_surcharge;
            let side_in = self.side_mut(asset_in);
            side_in.reserves.live_reserve = side_in
                .reserves
                .live_reserve
                .checked_add(retained)
                .ok_or(ErrorCode::ReserveOverflow)?;
            side_in.reserves.cash_reserve = side_in
                .reserves
                .cash_reserve
                .checked_add(retained)
                .ok_or(ErrorCode::ReserveOverflow)?;
            let evaluation = self.evaluate_current_amm_liquidity(current_slot)?;
            let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
            self.amm.commit_invariant(evaluation.invariant_d)?;
            self.amm.checkpoint_retained_surcharge(q_per_share_nad)?;
        }

        let (side_in, side_out) = self.swap_sides_mut(asset_in);
        let fees = side_in.record_claimable_swap_fees(
            swap_fee_credit.base,
            swap_fee_credit.distributed_surcharge,
            protocol_fee_bps,
            protocol_auction_split,
            fee_eligible_ylp_supply,
        )?;
        side_in.assert_share_backing()?;
        side_out.assert_share_backing()?;
        side_in.fees.assert_backed()?;
        Ok(fees)
    }

    fn record_leverage_borrow(&mut self, debt_asset: MarketAsset, gross_debt: u64, current_slot: u64) -> Result<()> {
        require_gte!(
            self.side(debt_asset).reserves.cash_reserve,
            gross_debt,
            ErrorCode::InsufficientBorrowHeadroom
        );
        self.record_new_borrow(debt_asset, gross_debt, current_slot)?;
        let debt_side = self.side_mut(debt_asset);
        debt_side.reserves.cash_reserve = debt_side
            .reserves
            .cash_reserve
            .checked_sub(gross_debt)
            .ok_or(ErrorCode::CashReserveUnderflow)?;
        Ok(())
    }

    fn add_isolated_borrow_debt(&mut self, debt_asset: MarketAsset, cash_debit: u64) -> Result<u128> {
        let aggregate_debt_before = self.debt.isolated_debt(debt_asset)?;
        let shares = self.debt.add_isolated_debt(debt_asset, cash_debit)?;
        let aggregate_debt_after = self.debt.isolated_debt(debt_asset)?;
        let aggregate_debt_increase = u64::try_from(
            aggregate_debt_after
                .checked_sub(aggregate_debt_before)
                .ok_or(ErrorCode::DebtMathOverflow)?,
        )
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let side = self.side_mut(debt_asset);
        if aggregate_debt_increase > cash_debit {
            side.reserves.live_reserve = side
                .reserves
                .live_reserve
                .checked_add(aggregate_debt_increase - cash_debit)
                .ok_or(ErrorCode::ReserveOverflow)?;
        } else if aggregate_debt_increase < cash_debit {
            side.reserves.live_reserve = side
                .reserves
                .live_reserve
                .checked_sub(cash_debit - aggregate_debt_increase)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        Ok(shares)
    }
}

pub(crate) fn leverage_debt_from_margin(margin_amount: u64, multiplier_bps: u64) -> Result<u64> {
    let notional = (margin_amount as u128)
        .checked_mul(multiplier_bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt = notional
        .checked_sub(margin_amount as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(debt > 0, ErrorCode::AmountZero);
    u64::try_from(debt).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn equity_bps(closeout_value: u64, debt_amount: u64) -> Result<u128> {
    if closeout_value == 0 {
        return Ok(0);
    }
    Ok((closeout_value.saturating_sub(debt_amount) as u128)
        .checked_mul(BPS_DENOMINATOR as u128)
        .and_then(|value| value.checked_div(closeout_value as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?)
}

fn require_initial_leverage_health(
    market: &Market,
    collateral_asset: MarketAsset,
    collateral_amount: u64,
    base_price_nad: u64,
    closeout_value: u64,
    debt_amount: u64,
) -> Result<()> {
    require_gt!(closeout_value, debt_amount, ErrorCode::LeverageInitialMarginTooLow);
    let margin_bps = equity_bps(closeout_value, debt_amount)?;
    require_gte!(
        margin_bps,
        LEVERAGE_INITIAL_MARGIN_BPS as u128,
        ErrorCode::LeverageInitialMarginTooLow
    );
    require!(base_price_nad > 0, ErrorCode::InsufficientLiquidity);
    let collateral_nad = normalize_to_nad(collateral_amount as u128, market.side(collateral_asset).asset_decimals)?;
    let spot_value_nad = match collateral_asset {
        MarketAsset::Base => collateral_nad
            .checked_mul(base_price_nad as u128)
            .and_then(|value| value.checked_div(crate::constants::NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
        MarketAsset::Quote => collateral_nad
            .checked_mul(crate::constants::NAD as u128)
            .and_then(|value| value.checked_div(base_price_nad as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
    };
    let spot_value =
        denormalize_from_nad_floor(spot_value_nad, market.side(collateral_asset.opposite()).asset_decimals)?;
    require!(spot_value > 0, ErrorCode::InsufficientLiquidity);
    let unwind_bps = if closeout_value >= spot_value {
        0
    } else {
        (spot_value as u128)
            .checked_sub(closeout_value as u128)
            .and_then(|value| value.checked_mul(BPS_DENOMINATOR as u128))
            .and_then(|value| value.checked_div(spot_value as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?
    };
    require_gte!(
        LEVERAGE_MAX_UNWIND_IMPACT_BPS as u128,
        unwind_bps,
        ErrorCode::LeverageUnwindImpactTooHigh
    );
    Ok(())
}

fn require_leverage_not_liquidatable(closeout_value: u64, debt_amount: u64) -> Result<()> {
    let margin_bps = equity_bps(closeout_value, debt_amount)?;
    require!(
        closeout_value > debt_amount && margin_bps > LEVERAGE_MAINTENANCE_BUFFER_BPS as u128,
        ErrorCode::LeveragePositionNotLiquidatable
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/leverage.rs");
}
