use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    math::realized_interest_split,
    state::*,
    transitions::{flash_liquidation::LiquidationPositionRef, LeverageCollateralFee},
};

pub(crate) enum LiquidationPositionMut<'a> {
    Borrow(&'a mut BorrowPosition),
    Leverage(&'a mut LeveragePosition),
}

impl LiquidationPositionMut<'_> {
    pub(crate) fn as_ref(&self) -> LiquidationPositionRef<'_> {
        match self {
            Self::Borrow(p) => LiquidationPositionRef::Borrow(p),
            Self::Leverage(p) => LiquidationPositionRef::Leverage(p),
        }
    }

    pub(crate) fn observe(&mut self, asset: MarketAsset, eligible: bool, now: i64, slot: u64) -> Result<()> {
        match self {
            Self::Borrow(p) => p.distress_mut(asset).observe(eligible, now, slot),
            Self::Leverage(p) => p.distress.observe(eligible, now, slot),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LiquidationDebtAllocation {
    pub shares: u128,
    pub debt: u64,
    pub principal: u64,
    pub remaining_debt: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FlashAmmSettlementTerms {
    pub allocation: LiquidationDebtAllocation,
    pub repayment: u64,
    pub fee: LiquidationFeeAllocation,
    pub owner_surplus: u64,
    pub obligation: u64,
}

impl Market {
    /// Compute the exact debt-side obligation used by paired Dusk-AMM flash
    /// settlement. Preview and settle share this calculation so the buyer can
    /// bind slippage to the final debt-token payout, rather than the temporary
    /// collateral transfer made by `begin_flash_liquidation`.
    pub(crate) fn flash_amm_settlement_terms(
        &self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        quote: FlashLiquidationQuote,
        debt_fee: LeverageCollateralFee,
        insurance_balance: u64,
        insurance_target: u64,
    ) -> Result<FlashAmmSettlementTerms> {
        let allocation = self.liquidation_debt_allocation(position, asset, quote.debt_shares_to_burn)?;
        let budget = quote
            .repayment
            .checked_add(quote.fee.total)
            .and_then(|v| v.checked_add(quote.owner_surplus))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let repayment = if quote.full {
            budget.min(allocation.debt)
        } else {
            require_gte!(quote.repayment, allocation.debt, ErrorCode::InvalidLiquidationSession);
            allocation.debt
        };
        let fee = LiquidationFeeAllocation::quote(
            repayment,
            (budget - repayment).min(quote.fee.total),
            insurance_balance,
            insurance_target,
        );
        let owner_surplus = budget
            .checked_sub(repayment)
            .and_then(|v| v.checked_sub(fee.total))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let insurance_fee_debit = debt_fee.effective_gross_for_credit(fee.insurance)?;
        let revenue_fee_debit = debt_fee.effective_gross_for_credit(fee.lp + fee.protocol)?;
        let owner_debit = debt_fee.effective_gross_for_credit(owner_surplus)?;
        let obligation = repayment
            .checked_add(insurance_fee_debit)
            .and_then(|v| v.checked_add(revenue_fee_debit))
            .and_then(|v| v.checked_add(owner_debit))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(FlashAmmSettlementTerms {
            allocation,
            repayment,
            fee,
            owner_surplus,
            obligation,
        })
    }

    /// Derive the debt and principal removed from the current aggregate state.
    /// Other positions may change that state during a flash route. Never apply
    /// a cloned begin-state ledger to settle.
    pub(crate) fn liquidation_debt_allocation(
        &self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        shares: u128,
    ) -> Result<LiquidationDebtAllocation> {
        let position_shares = position.shares(asset);
        require!(shares > 0 && shares <= position_shares, ErrorCode::InsufficientDebt);
        let index = self.debt.borrow_index(asset);
        let (aggregate, principal) = match (position, asset) {
            (LiquidationPositionRef::Borrow(_), MarketAsset::Base) => {
                (self.debt.fixed_base_shares, self.debt.fixed_base_principal)
            }
            (LiquidationPositionRef::Borrow(_), MarketAsset::Quote) => {
                (self.debt.fixed_quote_shares, self.debt.fixed_quote_principal)
            }
            (LiquidationPositionRef::Leverage(_), MarketAsset::Base) => {
                (self.debt.isolated_base_shares, self.debt.isolated_base_principal)
            }
            (LiquidationPositionRef::Leverage(_), MarketAsset::Quote) => {
                (self.debt.isolated_quote_shares, self.debt.isolated_quote_principal)
            }
        };
        let debt = Debt::aggregate_debt_reduction_for_shares(aggregate, shares, index)?;
        let remaining_debt = Debt::shares_to_debt(position_shares - shares, index)?;
        let removed_principal = match position {
            LiquidationPositionRef::Borrow(p) => p.principal_for_share_burn(&self.debt, asset, shares, debt)?,
            LiquidationPositionRef::Leverage(p) => {
                if shares == position_shares {
                    u64::try_from(p.debt_principal).map_err(|_| ErrorCode::DebtMathOverflow)?
                } else {
                    let position_debt = Debt::shares_to_debt(position_shares, index)?;
                    let position_reduction =
                        u64::try_from(position_debt - remaining_debt).map_err(|_| ErrorCode::DebtMathOverflow)?;
                    realized_interest_split(position_reduction, position_debt, p.debt_principal.min(position_debt))?.0
                }
            }
        };
        require_gte!(principal, removed_principal, ErrorCode::DebtMathOverflow);
        // A share-floor phase can remove one more principal atom than the
        // aggregate debt delta. The live/curve reconciliation below accounts
        // for that atom; it must not become a negative interest payment.
        require!(
            u128::from(removed_principal) <= u128::from(debt) + 1,
            ErrorCode::DebtMathOverflow
        );
        Ok(LiquidationDebtAllocation {
            shares,
            debt,
            principal: removed_principal,
            remaining_debt: u64::try_from(remaining_debt).map_err(|_| ErrorCode::DebtMathOverflow)?,
        })
    }

    /// Apply a measured external recovery after the physical repayment and
    /// insurance transfers have succeeded. Token handlers separately move paid
    /// interest and fees into their custody accounts. No unpaid interest earns
    /// a fee, referral entitlement, or insurance credit.
    pub(crate) fn settle_flash_recovery(
        &mut self,
        position: &mut LiquidationPositionMut<'_>,
        asset: MarketAsset,
        allocation: LiquidationDebtAllocation,
        collateral_debit: u64,
        sale_recovery: u64,
        insurance_debit: u64,
        insurance_credit: u64,
        full: bool,
        eligibility_fee: LeverageCollateralFee,
        now: i64,
        slot: u64,
    ) -> Result<LiquidationLossAllocation> {
        let current = self.liquidation_debt_allocation(&position.as_ref(), asset, allocation.shares)?;
        require!(current == allocation, ErrorCode::InvalidLiquidationSession);
        let before_curve = self.curve_reserves_nad()?;
        let unpaid_before = self.public_unpaid_interest(asset)?;
        let collateral_before = position.as_ref().collateral(asset);
        require!(
            collateral_debit > 0 && collateral_debit <= collateral_before,
            ErrorCode::InsufficientAmount
        );
        if full {
            require_eq!(collateral_debit, collateral_before, ErrorCode::BrokenInvariant);
            require_eq!(allocation.remaining_debt, 0, ErrorCode::BrokenInvariant);
        } else {
            require!(
                collateral_debit < collateral_before && allocation.remaining_debt > 0,
                ErrorCode::BrokenInvariant
            );
            require_gte!(sale_recovery, allocation.debt, ErrorCode::InsufficientAmount);
            require_eq!(insurance_debit, 0, ErrorCode::BrokenInvariant);
            require_eq!(insurance_credit, 0, ErrorCode::BrokenInvariant);
        }
        require_gte!(insurance_debit, insurance_credit, ErrorCode::BrokenInvariant);
        let debt_principal = allocation.principal.min(allocation.debt);
        let loss = LiquidationLossAllocation::quote_with_coverage(
            debt_principal,
            allocation.debt,
            sale_recovery,
            insurance_credit,
            self.insurance.principal_coverage_bps,
        )?;
        require_eq!(
            loss.insurance_credit,
            insurance_credit,
            ErrorCode::InsuranceDrawExceeded
        );
        if insurance_debit > 0 {
            self.insurance.consume_draw(asset, insurance_debit, slot)?;
        }
        let principal_credit = loss
            .principal_repaid
            .checked_add(insurance_credit)
            .ok_or(ErrorCode::ReserveOverflow)?;
        let live_debit = allocation
            .debt
            .checked_sub(principal_credit)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        let (aggregate_shares, aggregate_principal) =
            match (matches!(position, LiquidationPositionMut::Leverage(_)), asset) {
                (false, MarketAsset::Base) => (&mut self.debt.fixed_base_shares, &mut self.debt.fixed_base_principal),
                (false, MarketAsset::Quote) => {
                    (&mut self.debt.fixed_quote_shares, &mut self.debt.fixed_quote_principal)
                }
                (true, MarketAsset::Base) => (
                    &mut self.debt.isolated_base_shares,
                    &mut self.debt.isolated_base_principal,
                ),
                (true, MarketAsset::Quote) => (
                    &mut self.debt.isolated_quote_shares,
                    &mut self.debt.isolated_quote_principal,
                ),
            };
        *aggregate_shares = aggregate_shares
            .checked_sub(allocation.shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *aggregate_principal = aggregate_principal
            .checked_sub(allocation.principal)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let advanced =
            self.settle_public_debt_reserves(asset, unpaid_before, loss.interest_paid, live_debit, principal_credit)?;
        self.apply_liquidation_position(position, asset, allocation, collateral_debit, full)?;
        if loss.principal_written_off > 0 || advanced.canceled > advanced.protocol_returned {
            self.finalize_amm_socialized_loss_and_observe_risk(slot)?;
        } else {
            self.finalize_debt_repayment(slot, before_curve)?;
        }
        self.observe_liquidation_settlement(
            position,
            asset,
            allocation.remaining_debt,
            full,
            eligibility_fee,
            now,
            slot,
        )?;
        Ok(loss)
    }

    /// The shared position half of settlement. The caller has already removed
    /// the matching aggregate debt, either against external cash or inside the
    /// atomic AMM reserve/debt kernel.
    pub(crate) fn apply_liquidation_position(
        &mut self,
        position: &mut LiquidationPositionMut<'_>,
        asset: MarketAsset,
        allocation: LiquidationDebtAllocation,
        collateral_debit: u64,
        full: bool,
    ) -> Result<()> {
        require_eq!(
            u64::try_from(Debt::shares_to_debt(
                position
                    .as_ref()
                    .shares(asset)
                    .checked_sub(allocation.shares)
                    .ok_or(ErrorCode::DebtShareMathOverflow)?,
                self.debt.borrow_index(asset)
            )?)
            .map_err(|_| ErrorCode::DebtMathOverflow)?,
            allocation.remaining_debt,
            ErrorCode::BrokenInvariant
        );
        if full {
            require_eq!(allocation.remaining_debt, 0, ErrorCode::BrokenInvariant);
            require_eq!(
                collateral_debit,
                position.as_ref().collateral(asset),
                ErrorCode::BrokenInvariant
            );
        }
        match position {
            LiquidationPositionMut::Borrow(p) => {
                *p.principal_mut(asset) = p
                    .principal(asset)
                    .checked_sub(allocation.principal)
                    .ok_or(ErrorCode::DebtMathOverflow)?;
                let (position_shares, collateral) = match asset {
                    MarketAsset::Base => (&mut p.fixed_base_shares, &mut p.quote_collateral),
                    MarketAsset::Quote => (&mut p.fixed_quote_shares, &mut p.base_collateral),
                };
                *position_shares = position_shares
                    .checked_sub(allocation.shares)
                    .ok_or(ErrorCode::DebtShareMathOverflow)?;
                *collateral = collateral
                    .checked_sub(collateral_debit)
                    .ok_or(ErrorCode::InsufficientAmount)?;
                if full {
                    p.set_liquidation_cf_bps(asset, 0);
                    p.clear_referral_binding(asset);
                }
            }
            LiquidationPositionMut::Leverage(p) => {
                self.reduce_leverage_collateral(p, collateral_debit)?;
                p.debt_shares = p
                    .debt_shares
                    .checked_sub(allocation.shares)
                    .ok_or(ErrorCode::DebtShareMathOverflow)?;
                p.debt_principal = p
                    .debt_principal
                    .checked_sub(u128::from(allocation.principal))
                    .ok_or(ErrorCode::DebtMathOverflow)?;
                if full {
                    require_eq!(p.debt_principal, 0, ErrorCode::BrokenInvariant);
                    p.margin_amount = 0;
                    p.open_notional = 0;
                    p.multiplier_bps = 0;
                    p.referral_partner = Pubkey::default();
                    p.referral_interest_share_bps = 0;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn observe_liquidation_settlement(
        &mut self,
        position: &mut LiquidationPositionMut<'_>,
        asset: MarketAsset,
        remaining_debt: u64,
        full: bool,
        eligibility_fee: LeverageCollateralFee,
        now: i64,
        slot: u64,
    ) -> Result<()> {
        if let LiquidationPositionMut::Borrow(p) = position {
            let contribution = self.debt_capped_global_health_contribution(
                asset,
                u128::from(remaining_debt),
                p.collateral(asset.opposite()),
                &self.risk,
            )?;
            self.reconcile_global_health_contribution(p, asset, contribution)?;
        }
        if full {
            position.observe(asset, false, now, slot)?;
        } else {
            let health = self.flash_liquidation_health(&position.as_ref(), asset, eligibility_fee)?;
            position.observe(asset, health.eligible()?, now, slot)?;
        }
        Ok(())
    }
}
