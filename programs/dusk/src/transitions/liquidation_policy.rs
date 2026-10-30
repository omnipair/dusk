//! Shared liquidation policy arithmetic and serialized quote components.
use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, DEFAULT_PROTOCOL_REVENUE_SHARE_BPS},
    errors::ErrorCode,
};

pub const LIQUIDATION_MINIMUM_DISCOUNT_BPS: u16 = 50;
pub const LIQUIDATION_RECOVERY_BUFFER_BPS: u16 = 200;
pub const LIQUIDATION_FEE_BPS: u16 = 100;
pub const LIQUIDATION_PROTOCOL_SHARE_BPS: u16 = DEFAULT_PROTOCOL_REVENUE_SHARE_BPS;
pub const PRINCIPAL_LOSS_COVERAGE_BPS: u16 = 7_500;

/// Linear symmetric-EMA collateral value and actual indexed debt, both in debt
/// token atoms. Execution prices never determine reference eligibility.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationHealth {
    pub collateral_value: u64,
    pub debt: u64,
    pub maintenance_bps: u16,
}

impl LiquidationHealth {
    pub fn validate(&self) -> Result<()> {
        require!(
            self.maintenance_bps > 0 && self.maintenance_bps < BPS_DENOMINATOR,
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }

    pub fn eligible(&self) -> Result<bool> {
        self.validate()?;
        Ok(self.debt > 0
            && (self.debt >= self.collateral_value
                || u128::from(self.collateral_value - self.debt) * u128::from(BPS_DENOMINATOR)
                    <= u128::from(self.collateral_value) * u128::from(self.maintenance_bps)))
    }

    pub fn emergency_allowed(&self) -> Result<bool> {
        self.validate()?;
        Ok(self.debt > 0
            && (self.debt >= self.collateral_value
                || u128::from(self.collateral_value - self.debt) * u128::from(BPS_DENOMINATOR) * 100
                    <= u128::from(self.collateral_value) * u128::from(self.maintenance_bps) * 70))
    }

    /// Fractional progress from maintenance to nonpositive equity, scaled by
    /// 10,000. Saturation makes an insolvent position immediately reach the cap.
    pub fn distress_progress_bps(&self) -> Result<u16> {
        self.validate()?;
        if self.debt == 0 {
            return Ok(0);
        }
        if self.debt >= self.collateral_value {
            return Ok(BPS_DENOMINATOR);
        }
        let required = u128::from(self.collateral_value) * u128::from(self.maintenance_bps);
        let equity = u128::from(self.collateral_value - self.debt) * u128::from(BPS_DENOMINATOR);
        Ok((required.saturating_sub(equity) * u128::from(BPS_DENOMINATOR) / required) as u16)
    }

    pub fn rates(&self) -> Result<LiquidationRates> {
        require!(self.eligible()?, ErrorCode::PositionNotLiquidatable);
        let progress = u64::from(self.distress_progress_bps()?);
        Ok(LiquidationRates {
            buyer_discount_bps: LIQUIDATION_MINIMUM_DISCOUNT_BPS + (450 * progress / 10_000) as u16,
            emergency_reward_bps: (100 * progress / 10_000) as u16,
        })
    }

    /// Compare equity minus the surviving position's maintenance requirement.
    /// Euclidean quotient/remainder comparison preserves fractional improvement
    /// without a potentially overflowing signed 128-bit triple product.
    pub fn improves(&self, after: Self) -> Result<bool> {
        self.validate()?;
        after.validate()?;
        if after.debt == 0 {
            return Ok(true);
        }
        if self.collateral_value == 0 || after.collateral_value == 0 {
            return Ok(false);
        }
        if self.maintenance_bps == after.maintenance_bps {
            return Ok(u128::from(self.debt) * u128::from(after.collateral_value)
                > u128::from(after.debt) * u128::from(self.collateral_value));
        }
        let score = |h: Self| {
            let value = i128::from(h.collateral_value);
            let numerator = value * i128::from(BPS_DENOMINATOR - h.maintenance_bps)
                - i128::from(h.debt) * i128::from(BPS_DENOMINATOR);
            (numerator.div_euclid(value), numerator.rem_euclid(value) as u128)
        };
        let before_score = score(*self);
        let after_score = score(after);
        Ok(after_score.0 > before_score.0
            || (after_score.0 == before_score.0
                && after_score.1 * u128::from(self.collateral_value)
                    > before_score.1 * u128::from(after.collateral_value)))
    }

    pub fn recovered(&self) -> Result<bool> {
        self.validate()?;
        if self.debt == 0 {
            return Ok(true);
        }
        if self.debt >= self.collateral_value {
            return Ok(false);
        }
        let target = u32::from(self.maintenance_bps) + u32::from(LIQUIDATION_RECOVERY_BUFFER_BPS);
        Ok(u128::from(self.collateral_value - self.debt) * 10_000
            >= u128::from(self.collateral_value) * u128::from(target))
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationRates {
    pub buyer_discount_bps: u16,
    pub emergency_reward_bps: u16,
}

/// Whole spendable atoms. Transfer-fee gross-ups are separate from the net fee
/// obligation. The collected fee is never counted again as debt repayment.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationFeeAllocation {
    pub total: u64,
    pub protocol: u64,
    pub insurance: u64,
    pub lp: u64,
}

impl LiquidationFeeAllocation {
    pub fn quote(net_repayment: u64, surplus_cap: u64, insurance_balance: u64, target: u64) -> Self {
        let total = (net_repayment / 100).min(surplus_cap);
        let protocol =
            (u128::from(total) * u128::from(LIQUIDATION_PROTOCOL_SHARE_BPS) / u128::from(BPS_DENOMINATOR)) as u64;
        let designated = total - protocol;
        let shortfall = target.saturating_sub(insurance_balance);
        let insurance = if target == 0 || shortfall == 0 {
            0
        } else if u128::from(insurance_balance) * 4 <= u128::from(target) * 3 {
            designated.min(shortfall)
        } else {
            ((u128::from(designated) * 4 * u128::from(shortfall) / u128::from(target)) as u64).min(shortfall)
        };
        Self {
            total,
            protocol,
            insurance,
            lp: designated - insurance,
        }
    }
}

pub fn liquidation_insurance_target(principal: u128, hlp_indexed_debt: u128) -> Result<u64> {
    let target = principal
        .div_ceil(20)
        .checked_add(hlp_indexed_debt.div_ceil(15))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(target).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

/// A completed debt leg. `sale_recovery` is net of the caller reward and actual
/// transfer costs. Insurance never pays unpaid borrower/leverage interest.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationLossAllocation {
    pub principal_repaid: u64,
    pub interest_paid: u64,
    pub insurance_credit: u64,
    pub principal_written_off: u64,
    pub interest_canceled: u64,
    pub surplus: u64,
}

impl LiquidationLossAllocation {
    pub fn quote(principal: u64, debt: u64, sale_recovery: u64, insurance_net_capacity: u64) -> Result<Self> {
        Self::quote_with_coverage(
            principal,
            debt,
            sale_recovery,
            insurance_net_capacity,
            PRINCIPAL_LOSS_COVERAGE_BPS,
        )
    }

    pub fn quote_with_coverage(
        principal: u64,
        debt: u64,
        sale_recovery: u64,
        insurance_net_capacity: u64,
        coverage_bps: u16,
    ) -> Result<Self> {
        require_gte!(debt, principal, ErrorCode::BrokenInvariant);
        require_gte!(
            PRINCIPAL_LOSS_COVERAGE_BPS,
            coverage_bps,
            ErrorCode::InvalidMarketConfig
        );
        let principal_repaid = principal.min(sale_recovery);
        let principal_gap = principal - principal_repaid;
        let insurance_credit =
            ((u128::from(principal_gap) * u128::from(coverage_bps) / 10_000) as u64).min(insurance_net_capacity);
        let interest = debt - principal;
        let interest_paid = interest.min(sale_recovery.saturating_sub(principal));
        Ok(Self {
            principal_repaid,
            interest_paid,
            insurance_credit,
            principal_written_off: principal_gap - insurance_credit,
            interest_canceled: interest - interest_paid,
            surplus: sale_recovery.saturating_sub(debt),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_only_incentives_cover_exact_boundaries_and_invalid_health() {
        for maintenance in [1, 700, 1200, 1700, 9999] {
            let value = 100_000_000_u64;
            let required = value * u64::from(maintenance) / 10_000;
            let boundary = LiquidationHealth {
                collateral_value: value,
                debt: value - required,
                maintenance_bps: maintenance,
            };
            assert!(boundary.eligible().unwrap());
            assert_eq!(
                boundary.rates().unwrap(),
                LiquidationRates {
                    buyer_discount_bps: 50,
                    emergency_reward_bps: 0
                }
            );
            assert!(!LiquidationHealth {
                debt: boundary.debt - 1,
                ..boundary
            }
            .eligible()
            .unwrap());
            assert!(LiquidationHealth {
                debt: boundary.debt - 1,
                ..boundary
            }
            .rates()
            .is_err());
            let emergency = LiquidationHealth {
                debt: value - required * 70 / 100,
                ..boundary
            };
            assert!(emergency.emergency_allowed().unwrap());
            assert_eq!(emergency.rates().unwrap().emergency_reward_bps, 30);
            assert!(!LiquidationHealth {
                debt: emergency.debt - 1,
                ..emergency
            }
            .emergency_allowed()
            .unwrap());
            let halfway = LiquidationHealth {
                debt: value - required / 2,
                ..boundary
            };
            assert_eq!(
                halfway.rates().unwrap(),
                LiquidationRates {
                    buyer_discount_bps: 275,
                    emergency_reward_bps: 50
                }
            );
            for debt in [value, value + 1, u64::MAX] {
                assert_eq!(
                    LiquidationHealth { debt, ..boundary }.rates().unwrap(),
                    LiquidationRates {
                        buyer_discount_bps: 500,
                        emergency_reward_bps: 100
                    }
                );
            }
            assert!(!LiquidationHealth { debt: 0, ..boundary }.eligible().unwrap());
        }
        for maintenance_bps in [0, 10_000, u16::MAX] {
            assert!(LiquidationHealth {
                collateral_value: 100,
                debt: 100,
                maintenance_bps
            }
            .rates()
            .is_err());
        }
        assert_eq!(
            LiquidationHealth {
                collateral_value: 0,
                debt: 1,
                maintenance_bps: 700
            }
            .rates()
            .unwrap()
            .buyer_discount_bps,
            500
        );
        assert!(LiquidationHealth {
            collateral_value: 0,
            debt: 0,
            maintenance_bps: 700
        }
        .rates()
        .is_err());
    }

    #[test]
    fn rates_are_bounded_and_monotone_with_health() {
        let mut previous = LiquidationRates::default();
        for debt in 930_001..=1_000_001 {
            let h = LiquidationHealth {
                collateral_value: 1_000_000,
                debt,
                maintenance_bps: 700,
            };
            let rates = h.rates().unwrap();
            assert!((50..=500).contains(&rates.buyer_discount_bps));
            assert!(rates.emergency_reward_bps <= 100);
            assert!(rates.buyer_discount_bps >= previous.buyer_discount_bps);
            assert!(rates.emergency_reward_bps >= previous.emergency_reward_bps);
            previous = rates;
        }
    }

    #[test]
    fn useful_partial_need_not_reach_recovery_target() {
        let before = LiquidationHealth {
            collateral_value: 1_000,
            debt: 950,
            maintenance_bps: 700,
        };
        let after = LiquidationHealth {
            collateral_value: 900,
            debt: 854,
            ..before
        };
        assert!(before.improves(after).unwrap());
        assert!(!after.recovered().unwrap());
        assert!(!before.improves(LiquidationHealth { debt: 855, ..after }).unwrap());
    }

    #[test]
    fn fee_taper_conserves_every_atom_and_never_distributes_existing_insurance() {
        for repayment in [0, 99, 100, 499, 500, u64::MAX] {
            let fee = LiquidationFeeAllocation::quote(repayment, u64::MAX, 0, 0);
            assert_eq!(fee.protocol, fee.total / 5);
            assert_eq!(fee.protocol + fee.insurance + fee.lp, fee.total);
        }
        for (balance, insurance, lp) in [
            (0, 800, 0),
            (7_500, 800, 0),
            (8_750, 400, 400),
            (10_000, 0, 800),
            (20_000, 0, 800),
        ] {
            assert_eq!(
                LiquidationFeeAllocation::quote(100_000, u64::MAX, balance, 10_000),
                LiquidationFeeAllocation {
                    total: 1_000,
                    protocol: 200,
                    insurance,
                    lp
                }
            );
        }
        let capped = LiquidationFeeAllocation::quote(100_000, 55, 0, 1_000);
        assert_eq!(
            (capped.total, capped.protocol, capped.insurance, capped.lp),
            (55, 11, 44, 0)
        );
        for target in [0, 1, 7, u64::MAX] {
            let f = LiquidationFeeAllocation::quote(u64::MAX, u64::MAX, target / 2, target);
            assert_eq!(f.protocol + f.insurance + f.lp, f.total);
            assert!(f.insurance <= target - target / 2);
        }
        assert_eq!(liquidation_insurance_target(100_000, 30_000).unwrap(), 7_000);
        assert_eq!(liquidation_insurance_target(1, 1).unwrap(), 2);
    }

    #[test]
    fn principal_first_waterfall_cancels_interest_and_caps_coverage() {
        let loss = LiquidationLossAllocation::quote(900, 950, 850, 1_000).unwrap();
        assert_eq!(
            loss,
            LiquidationLossAllocation {
                principal_repaid: 850,
                insurance_credit: 37,
                principal_written_off: 13,
                interest_canceled: 50,
                ..LiquidationLossAllocation::default()
            }
        );
        let partial_interest = LiquidationLossAllocation::quote(900, 950, 920, 1_000).unwrap();
        assert_eq!(partial_interest.insurance_credit, 0);
        assert_eq!(partial_interest.interest_paid, 20);
        assert_eq!(partial_interest.interest_canceled, 30);
        let limited = LiquidationLossAllocation::quote(900, 950, 850, 10).unwrap();
        assert_eq!((limited.insurance_credit, limited.principal_written_off), (10, 40));
        let solvent = LiquidationLossAllocation::quote(900, 950, 1_000, 0).unwrap();
        assert_eq!(
            (solvent.principal_repaid, solvent.interest_paid, solvent.surplus),
            (900, 50, 50)
        );
    }
}
