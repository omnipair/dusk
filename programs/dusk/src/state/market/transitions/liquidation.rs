use anchor_lang::prelude::*;

use crate::{
    constants::{
        BPS_DENOMINATOR, LIQUIDATION_CLOSE_FACTOR_BPS, LIQUIDATION_INCENTIVE_BPS, LIQUIDATION_INSURANCE_FUNDING_BPS,
        LIQUIDATION_MAX_INCENTIVE_BPS, LIQUIDATION_PENALTY_BPS, NAD,
    },
    errors::ErrorCode,
    math::{denormalize_from_nad_ceil, health_bps, normalize_to_nad},
    shared::math::ceil_div,
    state::{market::health::liquidation_health_floor_bps, BorrowPosition, Debt, Market, MarketAsset},
};

pub struct Liquidation {
    pub debt_asset: MarketAsset,
    pub repay_credit: u64,
    pub insurance_spent: u64,
    pub insurance_credit: u64,
    pub max_socialized_loss: u64,
    pub terms: LiquidationTerms,
    pub pricing: LiquidationPricing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidationPricing {
    PessimisticReserves,
    ReferencePrice { debt_per_collateral_price_nad: u64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiquidationTerms {
    pub liquidation_incentive_bps: u16,
    pub insurance_funding_bps: u16,
    pub total_penalty_bps: u16,
    pub max_repay_amount: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiquidationReceipt {
    pub repaid_amount: u64,
    pub interest_paid: u64,
    pub collateral_seized: u64,
    pub collateral_to_liquidator: u64,
    pub insurance_funded: u64,
    pub insurance_drawn: u64,
    pub socialized_loss: u64,
    pub remaining_debt: u128,
    pub remaining_global_health_contribution: u64,
    pub remaining_liquidation_cf_bps: u16,
    pub liquidation_incentive_bps: u16,
    pub insurance_funding_bps: u16,
    pub max_repay_amount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiquidationDebtClearance {
    shares_to_burn: u128,
    aggregate_debt_reduction: u64,
}

impl Liquidation {
    #[cfg(test)]
    pub fn new(
        debt_asset: MarketAsset,
        repay_credit: u64,
        insurance_spent: u64,
        insurance_credit: u64,
        max_socialized_loss: u64,
        terms: LiquidationTerms,
    ) -> Self {
        Self {
            debt_asset,
            repay_credit,
            insurance_spent,
            insurance_credit,
            max_socialized_loss,
            terms,
            pricing: LiquidationPricing::PessimisticReserves,
        }
    }

    pub fn new_with_pricing(
        debt_asset: MarketAsset,
        repay_credit: u64,
        insurance_spent: u64,
        insurance_credit: u64,
        max_socialized_loss: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
    ) -> Self {
        Self {
            debt_asset,
            repay_credit,
            insurance_spent,
            insurance_credit,
            max_socialized_loss,
            terms,
            pricing,
        }
    }

    pub fn apply(self, market: &mut Market, borrow_position: &mut BorrowPosition) -> Result<LiquidationReceipt> {
        let debt_before = position_debt(market, borrow_position, self.debt_asset)?;
        require_gte!(debt_before, self.repay_credit as u128, ErrorCode::InsufficientDebt);
        require_gte!(
            self.terms.max_repay_amount,
            self.repay_credit,
            ErrorCode::LiquidationRepayTooLarge
        );
        let collateral_before = position_collateral(borrow_position, self.debt_asset);
        let collateral_seized = collateral_to_seize(
            market,
            self.debt_asset,
            self.repay_credit,
            collateral_before,
            self.terms.total_penalty_bps,
            self.pricing,
        )?;
        let collateral_to_liquidator = collateral_amount_for_debt_value_with_pricing(
            market,
            self.debt_asset,
            self.repay_credit,
            self.terms.liquidation_incentive_bps,
            self.pricing,
        )?
        .min(collateral_seized);
        let insurance_funded = collateral_seized
            .checked_sub(collateral_to_liquidator)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let collateral_exhausted = collateral_seized == collateral_before;
        let repay_plus_insurance = (self.repay_credit as u128)
            .checked_add(self.insurance_credit as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(debt_before, repay_plus_insurance, ErrorCode::InsufficientDebt);
        let cap_remaining = self
            .terms
            .max_repay_amount
            .checked_sub(self.repay_credit)
            .ok_or(ErrorCode::LiquidationRepayTooLarge)?;
        require_gte!(
            cap_remaining,
            self.insurance_credit,
            ErrorCode::LiquidationRepayTooLarge
        );

        let debt_clearance = {
            let (shares_before, position_debt_before, borrow_index_nad) = match self.debt_asset {
                MarketAsset::Base => (
                    borrow_position.fixed_base_shares,
                    borrow_position.fixed_base_debt(&market.debt)?,
                    market.debt.base_borrow_index_nad,
                ),
                MarketAsset::Quote => (
                    borrow_position.fixed_quote_shares,
                    borrow_position.fixed_quote_debt(&market.debt)?,
                    market.debt.quote_borrow_index_nad,
                ),
            };
            require!(
                shares_before > 0 && position_debt_before > 0,
                ErrorCode::InsufficientDebt
            );
            let repayment = if collateral_exhausted {
                Debt::repayment_for_max(
                    shares_before,
                    match self.debt_asset {
                        MarketAsset::Base => market.debt.fixed_base_shares,
                        MarketAsset::Quote => market.debt.fixed_quote_shares,
                    },
                    borrow_index_nad,
                    u64::MAX,
                )?
            } else {
                let cash_repaid = u64::try_from(repay_plus_insurance).map_err(|_| ErrorCode::MarketMathOverflow)?;
                let quote = Debt::repayment_for_max(
                    shares_before,
                    match self.debt_asset {
                        MarketAsset::Base => market.debt.fixed_base_shares,
                        MarketAsset::Quote => market.debt.fixed_quote_shares,
                    },
                    borrow_index_nad,
                    cash_repaid,
                )?;
                require_eq!(quote.cash_repaid, cash_repaid, ErrorCode::DebtShareDivisionOverflow);
                quote
            };
            LiquidationDebtClearance {
                shares_to_burn: repayment.shares_to_burn,
                aggregate_debt_reduction: repayment.cash_repaid,
            }
        };
        let cash_repaid = u64::try_from(repay_plus_insurance).map_err(|_| ErrorCode::MarketMathOverflow)?;
        require_gte!(
            debt_clearance.aggregate_debt_reduction,
            cash_repaid,
            ErrorCode::MarketMathOverflow
        );
        let socialized_loss = if collateral_exhausted {
            debt_clearance
                .aggregate_debt_reduction
                .checked_sub(cash_repaid)
                .ok_or(ErrorCode::MarketMathOverflow)?
        } else {
            0
        };
        require_gte!(
            self.max_socialized_loss,
            socialized_loss,
            ErrorCode::LiquidationSocializationExceeded
        );
        // Track the principal/interest split for cash-backed repayment without
        // treating socialized loss or share-rounding writeoff as received
        // interest.
        let interest_paid = market.debt.realize_margin_liquidation(
            self.debt_asset,
            cash_repaid,
            debt_clearance.aggregate_debt_reduction,
        )?;
        let principal_credit = cash_repaid
            .checked_sub(interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        match self.debt_asset {
            MarketAsset::Base => {
                borrow_position.quote_collateral = borrow_position
                    .quote_collateral
                    .checked_sub(collateral_seized)
                    .ok_or(ErrorCode::InsufficientBalance)?;
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                market.debt.fixed_base_shares = market
                    .debt
                    .fixed_base_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            MarketAsset::Quote => {
                borrow_position.base_collateral = borrow_position
                    .base_collateral
                    .checked_sub(collateral_seized)
                    .ok_or(ErrorCode::InsufficientBalance)?;
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                market.debt.fixed_quote_shares = market
                    .debt
                    .fixed_quote_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }

        {
            let debt_side = market.side_mut(self.debt_asset);
            let live_debit = debt_clearance
                .aggregate_debt_reduction
                .checked_sub(principal_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_sub(live_debit)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            debt_side.reserves.cash_reserve = debt_side
                .reserves
                .cash_reserve
                .checked_add(principal_credit)
                .ok_or(ErrorCode::ReserveOverflow)?;
        }
        match self.debt_asset {
            MarketAsset::Base => {
                market.insurance.base_available = market
                    .insurance
                    .base_available
                    .checked_sub(self.insurance_spent)
                    .ok_or(ErrorCode::InsufficientInsurance)?;
                market.insurance.quote_available = market
                    .insurance
                    .quote_available
                    .checked_add(insurance_funded)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            MarketAsset::Quote => {
                market.insurance.quote_available = market
                    .insurance
                    .quote_available
                    .checked_sub(self.insurance_spent)
                    .ok_or(ErrorCode::InsufficientInsurance)?;
                market.insurance.base_available = market
                    .insurance
                    .base_available
                    .checked_add(insurance_funded)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }

        market.refresh_risk()?;
        let remaining_debt = position_debt(market, borrow_position, self.debt_asset)?;
        let remaining_collateral = position_collateral(borrow_position, self.debt_asset);
        let target_contribution = market.debt_capped_global_health_contribution(
            self.debt_asset,
            remaining_debt,
            remaining_collateral,
            &market.risk,
        )?;
        if remaining_debt == 0 {
            borrow_position.set_liquidation_cf_bps(self.debt_asset, 0);
            borrow_position.clear_referral_binding(self.debt_asset);
        } else {
            let total_debt_nad = market.total_fixed_debt_nad(self.debt_asset)?;
            let external_debt_nad = market.external_fixed_debt_nad(borrow_position, self.debt_asset)?;
            let projected_aggregate = market.projected_aggregate_global_health_contribution(
                borrow_position,
                self.debt_asset,
                target_contribution,
            )?;
            let terms = market.dynamic_borrow_terms(
                self.debt_asset,
                remaining_collateral,
                external_debt_nad,
                total_debt_nad,
                projected_aggregate,
                &market.risk,
            )?;
            borrow_position.set_liquidation_cf_bps(self.debt_asset, terms.liquidation_cf_bps);
        }
        market.reconcile_global_health_contribution(borrow_position, self.debt_asset, target_contribution)?;
        market.reconcile_liquidation_auction(borrow_position)?;

        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;

        Ok(LiquidationReceipt {
            repaid_amount: self.repay_credit,
            interest_paid,
            collateral_seized,
            collateral_to_liquidator,
            insurance_funded,
            insurance_drawn: self.insurance_credit,
            socialized_loss,
            remaining_debt: position_debt(market, borrow_position, self.debt_asset)?,
            remaining_global_health_contribution: borrow_position.global_health_contribution(self.debt_asset),
            remaining_liquidation_cf_bps: borrow_position.liquidation_cf_bps(self.debt_asset),
            liquidation_incentive_bps: self.terms.liquidation_incentive_bps,
            insurance_funding_bps: self.terms.insurance_funding_bps,
            max_repay_amount: self.terms.max_repay_amount,
        })
    }
}

pub(crate) fn liquidation_terms_with_incentive_and_pricing(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    liquidation_incentive_bps: u16,
    pricing: LiquidationPricing,
) -> Result<LiquidationTerms> {
    let health_before = liquidation_health_bps_with_pricing(market, borrow_position, debt_asset, pricing)?;
    let liquidation_health_floor_bps = liquidation_health_floor_bps(borrow_position.liquidation_cf_bps(debt_asset));
    let max_incentive_bps = liquidation_max_incentive_bps(health_before, liquidation_health_floor_bps);
    require_gte!(
        max_incentive_bps,
        liquidation_incentive_bps,
        ErrorCode::InvalidMarketConfig
    );
    let max_total_penalty = liquidation_health_floor_bps.saturating_sub(BPS_DENOMINATOR as u64 + 1);
    let remaining_penalty_room = max_total_penalty.saturating_sub(liquidation_incentive_bps as u64);
    let insurance_funding_bps =
        LIQUIDATION_INSURANCE_FUNDING_BPS.min(u16::try_from(remaining_penalty_room).unwrap_or(u16::MAX));
    let total_penalty_bps = liquidation_incentive_bps
        .checked_add(insurance_funding_bps)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let max_repay_amount = {
        let debt_before = position_debt(market, borrow_position, debt_asset)?;
        if debt_before == 0 {
            0
        } else {
            let debt_decimals = market.side(debt_asset).asset_decimals;
            let debt_value_nad = normalize_to_nad(debt_before, debt_decimals)?;
            let collateral_value_nad =
                position_collateral_value_with_pricing(market, borrow_position, debt_asset, pricing)?;
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
                denormalize_from_nad_ceil(repay_value_nad, debt_decimals)?
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
                    market
                        .fixed_repayment_for_max(borrow_position, debt_asset, max_repay)
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
        insurance_funding_bps,
        total_penalty_bps,
        max_repay_amount,
    })
}

fn position_debt(market: &Market, borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> Result<u128> {
    match debt_asset {
        MarketAsset::Base => borrow_position.fixed_base_debt(&market.debt),
        MarketAsset::Quote => borrow_position.fixed_quote_debt(&market.debt),
    }
}

fn position_collateral(borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> u64 {
    match debt_asset {
        MarketAsset::Base => borrow_position.quote_collateral,
        MarketAsset::Quote => borrow_position.base_collateral,
    }
}

fn collateral_to_seize(
    market: &Market,
    debt_asset: MarketAsset,
    repay_credit: u64,
    collateral_before: u64,
    total_penalty_bps: u16,
    pricing: LiquidationPricing,
) -> Result<u64> {
    let seizure =
        collateral_amount_for_debt_value_with_pricing(market, debt_asset, repay_credit, total_penalty_bps, pricing)?;
    Ok(seizure.min(collateral_before))
}

pub(crate) fn liquidation_max_incentive_bps(health_bps: u64, min_health_bps: u64) -> u16 {
    let shortfall = min_health_bps.saturating_sub(health_bps);
    let max_for_config = min_health_bps
        .saturating_sub(BPS_DENOMINATOR as u64 + 1)
        .min(LIQUIDATION_MAX_INCENTIVE_BPS as u64);
    shortfall.max(LIQUIDATION_INCENTIVE_BPS as u64).min(max_for_config) as u16
}

pub(crate) fn liquidation_health_bps_with_pricing(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    pricing: LiquidationPricing,
) -> Result<u64> {
    let collateral_value_nad = position_collateral_value_with_pricing(market, borrow_position, debt_asset, pricing)?;
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
    health_bps(collateral_value_nad, normalize_to_nad(debt_before, debt_decimals)?)
}

#[cfg(test)]
fn max_repay_to_restore_health_with_pricing(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    total_penalty_bps: u16,
    pricing: LiquidationPricing,
) -> Result<u64> {
    let debt_before = position_debt(market, borrow_position, debt_asset)?;
    let debt_decimals = market.side(debt_asset).asset_decimals;
    let debt_value_nad = normalize_to_nad(debt_before, debt_decimals)?;
    let collateral_value_nad = position_collateral_value_with_pricing(market, borrow_position, debt_asset, pricing)?;
    let target_bps = liquidation_health_floor_bps(borrow_position.liquidation_cf_bps(debt_asset)) as u128;
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
    if target_debt_value <= collateral_value {
        return Ok(0);
    }
    let repay_value_nad = ceil_div(
        target_debt_value
            .checked_sub(collateral_value)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        target_bps
            .checked_sub(penalty_multiplier_bps)
            .ok_or(ErrorCode::MarketMathOverflow)?,
    )
    .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(denormalize_from_nad_ceil(repay_value_nad, debt_decimals)?.min(u64::try_from(debt_before).unwrap_or(u64::MAX)))
}

fn position_collateral_value_with_pricing(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
    pricing: LiquidationPricing,
) -> Result<u128> {
    match pricing {
        LiquidationPricing::PessimisticReserves => {
            let risk = market.current_risk()?;
            match debt_asset {
                MarketAsset::Base => {
                    market.liquidation_collateral_value_nad(MarketAsset::Quote, borrow_position.quote_collateral, &risk)
                }
                MarketAsset::Quote => {
                    market.liquidation_collateral_value_nad(MarketAsset::Base, borrow_position.base_collateral, &risk)
                }
            }
        }
        LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad,
        } => {
            let collateral_asset = debt_asset.opposite();
            let collateral_amount = position_collateral(borrow_position, debt_asset);
            require!(debt_per_collateral_price_nad > 0, ErrorCode::InvalidSettlementPrice);
            let collateral_amount_nad =
                normalize_to_nad(collateral_amount as u128, market.side(collateral_asset).asset_decimals)?;
            collateral_amount_nad
                .checked_mul(debt_per_collateral_price_nad as u128)
                .and_then(|value| value.checked_div(NAD as u128))
                .ok_or(ErrorCode::MarketMathOverflow.into())
        }
    }
}

fn collateral_amount_for_debt_value_with_pricing(
    market: &Market,
    debt_asset: MarketAsset,
    debt_amount: u64,
    penalty_bps: u16,
    pricing: LiquidationPricing,
) -> Result<u64> {
    match pricing {
        LiquidationPricing::PessimisticReserves => {
            let risk = market.current_risk()?;
            require_gte!(
                LIQUIDATION_PENALTY_BPS,
                LIQUIDATION_INCENTIVE_BPS,
                ErrorCode::InvalidMarketConfig
            );
            let debt_with_penalty = ceil_div(
                (debt_amount as u128)
                    .checked_mul((BPS_DENOMINATOR + penalty_bps) as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let collateral_asset = debt_asset.opposite();
            let (curve, collateral_to_debt) = market.pessimistic_risk_curve(collateral_asset, &risk, true)?;
            let debt_amount_nad = normalize_to_nad(debt_with_penalty, market.side(debt_asset).asset_decimals)?;
            let collateral_amount_nad = curve.exact_out(debt_amount_nad, collateral_to_debt)?;
            denormalize_from_nad_ceil(collateral_amount_nad, market.side(collateral_asset).asset_decimals)
        }
        LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad,
        } => {
            require!(debt_per_collateral_price_nad > 0, ErrorCode::InvalidSettlementPrice);
            let debt_decimals = market.side(debt_asset).asset_decimals;
            let collateral_decimals = market.side(debt_asset.opposite()).asset_decimals;
            let debt_with_penalty = ceil_div(
                (debt_amount as u128)
                    .checked_mul((BPS_DENOMINATOR + penalty_bps) as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let debt_value_nad = normalize_to_nad(debt_with_penalty, debt_decimals)?;
            let collateral_amount_nad = ceil_div(
                debt_value_nad
                    .checked_mul(NAD as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                debt_per_collateral_price_nad as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            denormalize_from_nad_ceil(collateral_amount_nad, collateral_decimals)
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
            normalize_to_nad(collateral_amount as u128, self.side(collateral_asset).asset_decimals)?;
        require!(collateral_amount_nad > 0, ErrorCode::InvalidSettlementPrice);
        let collateral_value_nad = self.liquidation_collateral_value_nad(collateral_asset, collateral_amount, &risk)?;
        let price_nad = collateral_value_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(collateral_amount_nad))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let price_nad = u64::try_from(price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(price_nad > 0, ErrorCode::InvalidSettlementPrice);
        Ok(price_nad)
    }

    pub fn liquidation_health_bps_with_pricing(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        pricing: LiquidationPricing,
    ) -> Result<u64> {
        liquidation_health_bps_with_pricing(self, borrow_position, debt_asset, pricing)
    }

    pub fn liquidation_terms(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
    ) -> Result<LiquidationTerms> {
        self.liquidation_terms_with_pricing(borrow_position, debt_asset, LiquidationPricing::PessimisticReserves)
    }

    pub fn liquidation_terms_with_pricing(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        pricing: LiquidationPricing,
    ) -> Result<LiquidationTerms> {
        let health_before = liquidation_health_bps_with_pricing(self, borrow_position, debt_asset, pricing)?;
        let liquidation_health_floor_bps = liquidation_health_floor_bps(borrow_position.liquidation_cf_bps(debt_asset));
        let liquidation_incentive_bps = liquidation_max_incentive_bps(health_before, liquidation_health_floor_bps);
        liquidation_terms_with_incentive_and_pricing(
            self,
            borrow_position,
            debt_asset,
            liquidation_incentive_bps,
            pricing,
        )
    }

    pub fn liquidation_terms_with_incentive_and_pricing(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        liquidation_incentive_bps: u16,
        pricing: LiquidationPricing,
    ) -> Result<LiquidationTerms> {
        liquidation_terms_with_incentive_and_pricing(
            self,
            borrow_position,
            debt_asset,
            liquidation_incentive_bps,
            pricing,
        )
    }

    pub fn insurance_request_for_liquidation_with_terms_and_pricing(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        repay_credit: u64,
        max_insurance_draw: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
    ) -> Result<u64> {
        let debt_before = position_debt(self, borrow_position, debt_asset)?;
        require_gte!(debt_before, repay_credit as u128, ErrorCode::InsufficientDebt);
        require_gte!(
            terms.max_repay_amount,
            repay_credit,
            ErrorCode::LiquidationRepayTooLarge
        );
        let collateral_before = position_collateral(borrow_position, debt_asset);
        let collateral_seized = collateral_to_seize(
            self,
            debt_asset,
            repay_credit,
            collateral_before,
            terms.total_penalty_bps,
            pricing,
        )?;
        let remaining_debt = debt_before
            .checked_sub(repay_credit as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if collateral_seized < collateral_before || remaining_debt == 0 {
            return Ok(0);
        }
        let available = match debt_asset {
            MarketAsset::Base => self.insurance.base_available,
            MarketAsset::Quote => self.insurance.quote_available,
        };
        let remaining_debt_cap = u64::try_from(remaining_debt).unwrap_or(u64::MAX);
        let remaining_partial_cap = terms
            .max_repay_amount
            .checked_sub(repay_credit)
            .ok_or(ErrorCode::LiquidationRepayTooLarge)?;
        Ok(remaining_debt_cap
            .min(available)
            .min(max_insurance_draw)
            .min(remaining_partial_cap))
    }

    pub fn settle_liquidation(
        &mut self,
        borrow_position: &mut BorrowPosition,
        debt_asset: MarketAsset,
        repay_credit: u64,
        insurance_spent: u64,
        insurance_credit: u64,
        max_socialized_loss: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
    ) -> Result<LiquidationReceipt> {
        Liquidation::new_with_pricing(
            debt_asset,
            repay_credit,
            insurance_spent,
            insurance_credit,
            max_socialized_loss,
            terms,
            pricing,
        )
        .apply(self, borrow_position)
    }
}

#[cfg(test)]
mod tests {
    include!("../../../tests/transitions/liquidation.rs");
}
