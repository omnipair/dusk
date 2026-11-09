use super::*;

pub(crate) fn max_cf_bps_from_liquidation_cf(liquidation_cf_bps: u16) -> u16 {
    ((liquidation_cf_bps as u32).saturating_mul((BPS_DENOMINATOR - LTV_BUFFER_BPS) as u32) / BPS_DENOMINATOR as u32)
        as u16
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidationPricing {
    PessimisticReserves,
    ReferencePrice { debt_per_collateral_price_nad: u64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiquidationTerms {
    pub liquidation_incentive_bps: u16,
    pub junior_fee_bps: u16,
    pub total_penalty_bps: u16,
    pub max_repay_amount: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiquidationReceipt {
    pub repaid_amount: u64,
    pub interest_paid: u64,
    pub collateral_seized: u64,
    pub collateral_to_liquidator: u64,
    pub junior_fee_funded: u64,
    pub socialized_loss: u64,
    pub remaining_debt: u128,
    pub remaining_global_health_contribution: u64,
    pub remaining_liquidation_cf_bps: u16,
    pub liquidation_incentive_bps: u16,
    pub junior_fee_bps: u16,
    pub max_repay_amount: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InternalLiquidationReceipt {
    pub liquidation: LiquidationReceipt,
    pub owner_residual: u64,
}

fn position_collateral(borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> u64 {
    match debt_asset {
        MarketAsset::Base => borrow_position.quote_collateral,
        MarketAsset::Quote => borrow_position.base_collateral,
    }
}

pub(crate) fn liquidation_health_bps_with_pricing_and_credit(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    collateral_exit_credit: u64,
    pricing: LiquidationPricing,
) -> Result<u64> {
    let collateral_value_nad =
        position_collateral_value_with_pricing_amount(market, debt_asset, collateral_exit_credit, pricing)?;
    let (debt_before, debt_decimals) = match debt_asset {
        MarketAsset::Base => (
            borrow_position.fixed_base_debt(&market.debt)?,
            market.base_side.asset_decimals,
        ),
        MarketAsset::Quote => (
            borrow_position.fixed_quote_debt(&market.debt)?,
            market.quote_side.asset_decimals,
        ),
    };
    health_bps(
        collateral_value_nad,
        market.normalize_amount(debt_before, debt_decimals)?,
    )
}

fn position_collateral_value_with_pricing_amount(
    market: &Market,
    debt_asset: MarketAsset,
    collateral_amount: u64,
    pricing: LiquidationPricing,
) -> Result<u128> {
    match pricing {
        LiquidationPricing::PessimisticReserves => {
            let risk = market.current_risk()?;
            market.linear_liquidation_collateral_value_nad(debt_asset.opposite(), collateral_amount, &risk)
        }
        LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad,
        } => {
            let collateral_asset = debt_asset.opposite();
            require!(debt_per_collateral_price_nad > 0, ErrorCode::InvalidSettlementPrice);
            let collateral_amount_nad =
                market.normalize_amount(collateral_amount as u128, market.side(collateral_asset).asset_decimals)?;
            collateral_amount_nad
                .checked_mul(debt_per_collateral_price_nad as u128)
                .and_then(|value| value.checked_div(NAD as u128))
                .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
        }
    }
}

impl Market {
    pub fn liquidation_reference_price_nad(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
    ) -> Result<u64> {
        let risk = self.current_risk()?;
        let collateral_asset = debt_asset.opposite();
        let collateral_amount = position_collateral(borrow_position, debt_asset);
        let collateral_amount_nad =
            self.normalize_amount(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        require!(collateral_amount_nad > 0, ErrorCode::InvalidSettlementPrice);
        let (geometry, point, direction) = self
            .pessimistic_concentrated_curve(collateral_asset, &risk, false)?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let collateral_value_nad = geometry
            .quote_exact_in(point, collateral_amount_nad, direction)?
            .amount_out;
        let price_nad = collateral_value_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(collateral_amount_nad))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let price_nad = u64::try_from(price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(price_nad > 0, ErrorCode::InvalidSettlementPrice);
        Ok(price_nad)
    }

    pub fn liquidation_terms_with_pricing_and_credit(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        collateral_exit_credit: u64,
        pricing: LiquidationPricing,
    ) -> Result<LiquidationTerms> {
        require_gte!(
            position_collateral(borrow_position, debt_asset),
            collateral_exit_credit,
            ErrorCode::BrokenInvariant
        );
        let health_before = liquidation_health_bps_with_pricing_and_credit(
            self,
            borrow_position,
            debt_asset,
            collateral_exit_credit,
            pricing,
        )?;
        let liquidation_cf_bps = borrow_position.liquidation_cf_bps(debt_asset);
        let liquidation_health_floor_bps = if liquidation_cf_bps == 0 {
            u64::MAX
        } else {
            ceil_div((BPS_DENOMINATOR as u128).pow(2), liquidation_cf_bps as u128)
                .unwrap_or(u128::from(u64::MAX))
                .min(u128::from(u64::MAX)) as u64
        };
        let shortfall = liquidation_health_floor_bps.saturating_sub(health_before);
        let max_for_config = liquidation_health_floor_bps
            .saturating_sub(BPS_DENOMINATOR as u64 + 1)
            .min(LIQUIDATION_MAX_INCENTIVE_BPS as u64);
        let liquidation_incentive_bps = shortfall.max(LIQUIDATION_INCENTIVE_BPS as u64).min(max_for_config) as u16;
        let max_total_penalty = liquidation_health_floor_bps.saturating_sub(BPS_DENOMINATOR as u64 + 1);
        let remaining_penalty_room = max_total_penalty.saturating_sub(liquidation_incentive_bps as u64);
        let junior_fee_bps = LIQUIDATION_FEE_BPS.min(u16::try_from(remaining_penalty_room).unwrap_or(u16::MAX));
        let total_penalty_bps = liquidation_incentive_bps
            .checked_add(junior_fee_bps)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let max_repay_amount = {
            let debt_before = match debt_asset {
                MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
                MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
            };
            if debt_before == 0 {
                0
            } else {
                let debt_decimals = self.side(debt_asset).asset_decimals;
                let debt_value_nad = self.normalize_amount(debt_before, debt_decimals)?;
                let collateral_value_nad =
                    position_collateral_value_with_pricing_amount(self, debt_asset, collateral_exit_credit, pricing)?;
                let target_bps = liquidation_health_floor_bps as u128;
                let penalty_multiplier_bps = (BPS_DENOMINATOR as u128)
                    .checked_add(total_penalty_bps as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                require!(target_bps > penalty_multiplier_bps, ErrorCode::InvalidMarketConfig);
                let target_debt_value = debt_value_nad
                    .checked_mul(target_bps)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let collateral_value = collateral_value_nad
                    .checked_mul(BPS_DENOMINATOR as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let restore_cap = if target_debt_value <= collateral_value {
                    0
                } else {
                    let repay_value_nad = ceil_div(
                        target_debt_value
                            .checked_sub(collateral_value)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                        target_bps
                            .checked_sub(penalty_multiplier_bps)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                    )
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                    self.denormalize_amount_ceil(repay_value_nad, debt_decimals)?
                        .min(u64::try_from(debt_before).unwrap_or(u64::MAX))
                };
                if restore_cap == 0 {
                    0
                } else {
                    let close_factor_cap = ceil_div(
                        debt_before
                            .checked_mul(LIQUIDATION_CLOSE_FACTOR_BPS as u128)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                        BPS_DENOMINATOR as u128,
                    )
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                    let debt_before_u64 = u64::try_from(debt_before).unwrap_or(u64::MAX);
                    let mut max_repay = restore_cap.min(u64::try_from(close_factor_cap).unwrap_or(u64::MAX));
                    let leaves_dust = if max_repay as u128 >= debt_before {
                        false
                    } else if debt_before.saturating_sub(max_repay as u128) <= 1 {
                        true
                    } else {
                        self.fixed_repayment_for_max(borrow_position, debt_asset, max_repay)
                            .map(|repayment| {
                                repayment.shares_to_burn
                                    == match debt_asset {
                                        MarketAsset::Base => borrow_position.fixed_base_shares,
                                        MarketAsset::Quote => borrow_position.fixed_quote_shares,
                                    }
                            })
                            .unwrap_or(false)
                    };
                    if max_repay >= debt_before_u64 || leaves_dust {
                        max_repay = debt_before_u64;
                    }
                    max_repay
                }
            }
        };
        Ok(LiquidationTerms {
            liquidation_incentive_bps,
            junior_fee_bps,
            total_penalty_bps,
            max_repay_amount,
        })
    }

    /// Closes a lending position after its external auction expires. The AMM
    /// leg has already debited `swap_output` from executable reserves; this
    /// step applies that output to fixed debt, charges junior before senior,
    /// and writes off any remaining debt without a caller-selected veto. The
    /// interest and principal use their separate settlement ledgers.
    pub fn settle_internal_liquidation(
        &mut self,
        borrow_position: &mut BorrowPosition,
        debt_asset: MarketAsset,
        swap_output: u64,
        collateral_consumed: u64,
        caller_bounty: u64,
    ) -> Result<InternalLiquidationReceipt> {
        borrow_position.require_idle()?;
        let unpaid_before = self.public_unpaid_interest(debt_asset)?;
        let collateral_before = position_collateral(borrow_position, debt_asset);
        require_eq!(collateral_consumed, collateral_before, ErrorCode::BrokenInvariant);
        require_gte!(collateral_consumed, caller_bounty, ErrorCode::BrokenInvariant);

        let (position_shares, aggregate_shares, borrow_index_nad) = match debt_asset {
            MarketAsset::Base => (
                borrow_position.fixed_base_shares,
                self.debt.fixed_base_shares,
                self.debt.base_borrow_index_nad,
            ),
            MarketAsset::Quote => (
                borrow_position.fixed_quote_shares,
                self.debt.fixed_quote_shares,
                self.debt.quote_borrow_index_nad,
            ),
        };
        require!(position_shares > 0, ErrorCode::InsufficientDebt);
        let full_repayment = Debt::repayment_for_max(position_shares, aggregate_shares, borrow_index_nad, u64::MAX)?;
        require_eq!(
            full_repayment.shares_to_burn,
            position_shares,
            ErrorCode::BrokenInvariant
        );
        let repay_credit = swap_output.min(full_repayment.cash_repaid);
        let owner_residual = swap_output
            .checked_sub(repay_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let cash_repaid = repay_credit;
        let socialized_loss = full_repayment
            .cash_repaid
            .checked_sub(cash_repaid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let (interest_paid, principal_credit) = self.clear_fixed_position_principal(
            borrow_position,
            debt_asset,
            cash_repaid,
            position_shares,
            full_repayment.cash_repaid,
        )?;

        match debt_asset {
            MarketAsset::Base => {
                borrow_position.quote_collateral = 0;
                borrow_position.fixed_base_shares = 0;
                self.debt.fixed_base_shares = self
                    .debt
                    .fixed_base_shares
                    .checked_sub(position_shares)
                    .ok_or(ErrorCode::DebtShareMathOverflow)?;
            }
            MarketAsset::Quote => {
                borrow_position.base_collateral = 0;
                borrow_position.fixed_quote_shares = 0;
                self.debt.fixed_quote_shares = self
                    .debt
                    .fixed_quote_shares
                    .checked_sub(position_shares)
                    .ok_or(ErrorCode::DebtShareMathOverflow)?;
            }
        }
        let live_debit = full_repayment
            .cash_repaid
            .checked_sub(principal_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.settle_public_debt_reserves(debt_asset, unpaid_before, interest_paid, live_debit, principal_credit)?;
        borrow_position.set_liquidation_cf_bps(debt_asset, 0);
        borrow_position.clear_referral_binding(debt_asset);
        self.reconcile_global_health_contribution(borrow_position, debt_asset, 0)?;
        borrow_position.clear_liquidation_auction();

        Ok(InternalLiquidationReceipt {
            liquidation: LiquidationReceipt {
                repaid_amount: repay_credit,
                interest_paid,
                collateral_seized: collateral_consumed,
                collateral_to_liquidator: caller_bounty,
                junior_fee_funded: 0,
                socialized_loss,
                remaining_debt: 0,
                remaining_global_health_contribution: 0,
                remaining_liquidation_cf_bps: 0,
                liquidation_incentive_bps: LIQUIDATION_BACKSTOP_CALLER_BPS,
                junior_fee_bps: 0,
                max_repay_amount: full_repayment.cash_repaid,
            },
            owner_residual,
        })
    }
}

#[cfg(any(test, feature = "benchmark"))]
include!("../../../benchmark/analysis/liquidation_execution.rs");

#[cfg(any(test, feature = "benchmark"))]
include!("../../../benchmark/analysis/auction_liquidation.rs");

#[cfg(test)]
include!("../../tests/support/lending_liquidation.rs");

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/lending_liquidation.rs");
}
