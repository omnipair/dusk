use anchor_lang::prelude::*;

use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::mul_div_u128,
    state::*,
    transitions::{
        amm::{PreparedSwap, SwapRequest},
        flash_liquidation::LiquidationPositionRef,
        flash_settlement::{LiquidationDebtAllocation, LiquidationPositionMut},
        liquidity::SwapCashPolicy,
        LeverageCollateralFee, LeverageSwapFeeCredit,
    },
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct EmergencyLiquidationQuote {
    /// Symmetric collateral-price EMA (debt per collateral, NAD), not live spot.
    pub reference_price_nad: u64,
    /// Smoothed total active curve depth used only for settlement sizing.
    pub reference_depth_nad: u128,
    pub full: bool,
    pub collateral_debit: u64,
    pub collateral_credit: u64,
    pub swap_output: u64,
    pub repayment: u64,
    pub shares_to_burn: u128,
    pub reward_credit: u64,
    pub reward_debit: u64,
    pub fee: LiquidationFeeAllocation,
    pub insurance_fee_debit: u64,
    pub revenue_fee_debit: u64,
    pub owner_debit: u64,
    pub owner_credit: u64,
    pub insurance_debit: u64,
    pub loss: LiquidationLossAllocation,
    pub health_before: LiquidationHealth,
    pub health_after: LiquidationHealth,
    pub rates: LiquidationRates,
}

pub(crate) struct PreparedEmergencyLiquidation {
    pub quote: EmergencyLiquidationQuote,
    pub swap: Option<Box<PreparedSwap>>,
    pub allocation: LiquidationDebtAllocation,
    pub policy: SwapCashPolicy,
}

fn fee_debits(fee: LiquidationFeeAllocation, token_fee: LeverageCollateralFee) -> Result<(u64, u64)> {
    Ok((
        token_fee.effective_gross_for_credit(fee.insurance)?,
        token_fee.effective_gross_for_credit(fee.lp + fee.protocol)?,
    ))
}

impl Market {
    // Freeze an EMA-priced inventory settlement and bind its credited output
    // to debt repayment. Only real payouts need cash; virtual reference reserves
    // must never replace the real reserve or hLP accounting.
    pub(crate) fn prepare_emergency_liquidation(
        &mut self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        collateral_debit: u64,
        full: bool,
        collateral_fee: LeverageCollateralFee,
        eligibility_fee: LeverageCollateralFee,
        debt_fee: LeverageCollateralFee,
        now: i64,
        slot: u64,
        protocol_fee_bps: u16,
    ) -> Result<PreparedEmergencyLiquidation> {
        let before = self.liquidation_scratch();
        let prepared = self.prepare_emergency_candidate(
            position,
            asset,
            collateral_debit,
            full,
            collateral_fee,
            eligibility_fee,
            debt_fee,
            now,
            slot,
            protocol_fee_bps,
        )?;
        if !full && collateral_debit > 1 {
            require!(
                before.proves_partial_bound(
                    position,
                    asset,
                    collateral_fee,
                    eligibility_fee,
                    debt_fee,
                    now,
                    slot,
                    Some(collateral_debit)
                )?,
                ErrorCode::LiquidationRepayTooLarge
            );
        }
        Ok(prepared)
    }

    // Keep the prepared swap and candidate calculation out of the outer
    // recovery-proof frame on SBF.
    #[inline(never)]
    pub(crate) fn prepare_emergency_candidate(
        &mut self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        collateral_debit: u64,
        full: bool,
        collateral_fee: LeverageCollateralFee,
        eligibility_fee: LeverageCollateralFee,
        debt_fee: LeverageCollateralFee,
        now: i64,
        slot: u64,
        protocol_fee_bps: u16,
    ) -> Result<PreparedEmergencyLiquidation> {
        position.require_idle()?;
        let health_before = self.flash_liquidation_health(position, asset, eligibility_fee)?;
        require!(health_before.emergency_allowed()?, ErrorCode::PositionNotLiquidatable);
        let available = position.collateral(asset);
        require!(
            collateral_debit > 0 && collateral_debit <= available,
            ErrorCode::InsufficientAmount
        );
        if full {
            require_eq!(collateral_debit, available, ErrorCode::LiquidationRepayTooLarge);
            require!(
                health_before.debt >= health_before.collateral_value
                    // Full AMM fallback depends on whether a smaller AMM sale
                    // can restore MM + buffer, not whether an external buyer or
                    // a tiny below-target partial could improve health.
                    || self.proves_partial_bound(
                        position,
                        asset,
                        collateral_fee,
                        eligibility_fee,
                        debt_fee,
                        now,
                        slot,
                        Some(available)
                    )?,
                ErrorCode::LiquidationRepayTooLarge
            );
        } else {
            require!(collateral_debit < available, ErrorCode::LiquidationRepayTooLarge);
        }
        let rates = health_before.rates()?;
        let collateral_credit = collateral_fee.unwind_credit(collateral_debit)?;
        let mut swap = if collateral_credit == 0 {
            None
        } else {
            Some(
                SwapRequest {
                    current_slot: slot,
                    current_unix_timestamp: now,
                    asset_in: asset.opposite(),
                    reserve_credit: collateral_credit,
                    protocol_fee_bps,
                }
                .prepare_ema_liquidation(self)?,
            )
        };
        let output = swap.as_ref().map_or(0, |s| s.quote.amount_out);
        let reward_credit = u64::try_from(mul_div_u128(
            u128::from(debt_fee.unwind_credit(output)?),
            u128::from(rates.emergency_reward_bps),
            10_000,
        )?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let reward_debit = debt_fee.effective_gross_for_credit(reward_credit)?;
        let budget = output.checked_sub(reward_debit).ok_or(ErrorCode::InsufficientAmount)?;
        let target = self.liquidation_insurance_target(asset)?;
        let balance = self.insurance.available(asset);
        let (allocation, repayment, fee) = if full {
            let allocation = self.liquidation_debt_allocation(position, asset, position.shares(asset))?;
            let repayment = allocation.debt.min(budget);
            let surplus = budget - repayment;
            let (mut low, mut high) = (0, (repayment / 100).min(surplus));
            while low < high {
                let cap = low + (high - low).div_ceil(2);
                let fee = LiquidationFeeAllocation::quote(repayment, cap, balance, target);
                let (insurance, revenue) = fee_debits(fee, debt_fee)?;
                if u128::from(insurance) + u128::from(revenue) <= u128::from(surplus) {
                    low = cap;
                } else {
                    high = cap - 1;
                }
            }
            (
                allocation,
                repayment,
                LiquidationFeeAllocation::quote(repayment, low, balance, target),
            )
        } else {
            let (allocation, fee) = self.quote_emergency_partial_repayment(position, asset, budget, debt_fee)?;
            (allocation, allocation.debt, fee)
        };
        let remaining = available - collateral_debit;
        let health_after = LiquidationHealth {
            collateral_value: self.liquidation_value(asset, eligibility_fee.unwind_credit(remaining)?)?,
            debt: allocation.remaining_debt,
            maintenance_bps: position.maintenance(asset, remaining.max(1))?,
        };
        if !full {
            require!(
                health_before.improves(health_after)?,
                ErrorCode::LiquidationDoesNotImproveHealth
            );
        }
        let net_capacity = if full {
            debt_fee.unwind_credit(self.insurance.draw_capacity(asset, slot)?)?
        } else {
            0
        };
        let loss = LiquidationLossAllocation::quote_with_coverage(
            allocation.principal.min(allocation.debt),
            allocation.debt,
            repayment,
            net_capacity,
            self.insurance.principal_coverage_bps,
        )?;
        let insurance_debit = debt_fee.effective_gross_for_credit(loss.insurance_credit)?;
        let (insurance_fee_debit, revenue_fee_debit) = fee_debits(fee, debt_fee)?;
        let owner_debit = budget
            .checked_sub(repayment)
            .and_then(|v| v.checked_sub(insurance_fee_debit))
            .and_then(|v| v.checked_sub(revenue_fee_debit))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let policy = SwapCashPolicy::SettleLiquidation {
            debt_asset: asset,
            isolated: matches!(position, LiquidationPositionRef::Leverage(_)),
            full,
            shares: allocation.shares,
            principal_removed: allocation.principal,
            debt_reduced: allocation.debt,
            repayment,
            insurance_credit: loss.insurance_credit,
        };
        if let Some(swap) = &mut swap {
            swap.bind_liquidation(self, policy)?;
        }
        let reference = self.current_risk()?;
        let quote = EmergencyLiquidationQuote {
            reference_price_nad: self.pessimistic_collateral_price_nad(asset.opposite(), &reference, false),
            reference_depth_nad: reference.curve_depth_ema_nad,
            full,
            collateral_debit,
            collateral_credit,
            swap_output: output,
            repayment,
            shares_to_burn: allocation.shares,
            reward_credit,
            reward_debit,
            fee,
            insurance_fee_debit,
            revenue_fee_debit,
            owner_debit,
            owner_credit: debt_fee.unwind_credit(owner_debit)?,
            insurance_debit,
            loss,
            health_before,
            health_after,
            rates,
        };
        Ok(PreparedEmergencyLiquidation {
            quote,
            swap,
            allocation,
            policy,
        })
    }

    // The full-close certificate and actual AMM execution must use identical
    // integer share burns and transfer-fee gross-ups. This calculation does
    // not materialize a swap, allocate account clones, or mutate any state.
    pub(crate) fn quote_emergency_partial_repayment(
        &self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        budget: u64,
        debt_fee: LeverageCollateralFee,
    ) -> Result<(LiquidationDebtAllocation, LiquidationFeeAllocation)> {
        let target = self.liquidation_insurance_target(asset)?;
        let balance = self.insurance.available(asset);
        let maximum = position
            .shares(asset)
            .checked_sub(1)
            .ok_or(ErrorCode::InsufficientDebt)?;
        let (mut low, mut high) = (
            0,
            maximum.min(mul_div_u128(
                u128::from(budget),
                u128::from(NAD),
                self.debt.borrow_index(asset),
            )?),
        );
        while low < high {
            let shares = low + (high - low).div_ceil(2);
            let allocation = self.liquidation_debt_allocation(position, asset, shares)?;
            let fee = LiquidationFeeAllocation::quote(allocation.debt, u64::MAX, balance, target);
            let (insurance, revenue) = fee_debits(fee, debt_fee)?;
            if u128::from(allocation.debt) + u128::from(insurance) + u128::from(revenue) <= u128::from(budget) {
                low = shares;
            } else {
                high = shares - 1;
            }
        }
        require_gt!(low, 0, ErrorCode::InsufficientAmount);
        let allocation = self.liquidation_debt_allocation(position, asset, low)?;
        let fee = LiquidationFeeAllocation::quote(allocation.debt, u64::MAX, balance, target);
        Ok((allocation, fee))
    }

    pub(crate) fn apply_emergency_liquidation(
        &mut self,
        position: &mut LiquidationPositionMut<'_>,
        asset: MarketAsset,
        prepared: &mut PreparedEmergencyLiquidation,
        slot: u64,
        protocol_fee_bps: u16,
        split: ProtocolAuctionSplit,
    ) -> Result<Option<crate::transitions::amm::FinalizedSwapState>> {
        let q = prepared.quote;
        require!(
            self.liquidation_debt_allocation(&position.as_ref(), asset, prepared.allocation.shares)?
                == prepared.allocation,
            ErrorCode::InvalidLiquidationSession
        );
        if let Some(swap) = &mut prepared.swap {
            let fee_credit = LeverageSwapFeeCredit::from_total_actual_credit(
                &swap.leverage_quote(),
                swap.quote.fee.claimable_fee_debit,
            )?;
            let state = swap.apply(self, prepared.policy, fee_credit, slot, protocol_fee_bps, split, None)?;
            if q.insurance_debit > 0 {
                self.insurance.consume_draw(asset, q.insurance_debit, slot)?;
            }
            self.apply_liquidation_position(position, asset, prepared.allocation, q.collateral_debit, q.full)?;
            self.reconcile_liquidation_position(position, asset, prepared.allocation.remaining_debt)?;
            Ok(Some(state))
        } else {
            require!(q.full && q.swap_output == 0, ErrorCode::BrokenInvariant);
            let loss = self.settle_flash_recovery(
                position,
                asset,
                prepared.allocation,
                q.collateral_debit,
                0,
                q.insurance_debit,
                q.loss.insurance_credit,
                true,
                slot,
            )?;
            require!(loss == q.loss, ErrorCode::BrokenInvariant);
            Ok(None)
        }
    }
}
