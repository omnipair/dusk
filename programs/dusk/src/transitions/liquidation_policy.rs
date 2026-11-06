//! Shared liquidation policy arithmetic and serialized quote components.
use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, DEFAULT_PROTOCOL_REVENUE_SHARE_BPS},
    errors::ErrorCode,
};

pub const LIQUIDATION_MINIMUM_DISCOUNT_BPS: u16 = 50;
pub const LIQUIDATION_RECOVERY_BUFFER_BPS: u16 = 200;
pub use crate::constants::LIQUIDATION_FEE_BPS;
pub const LIQUIDATION_PROTOCOL_SHARE_BPS: u16 = DEFAULT_PROTOCOL_REVENUE_SHARE_BPS;

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
    pub junior: u64,
}

impl LiquidationFeeAllocation {
    /// A fixed 3% charge, bounded by recovered surplus. The protocol share is
    /// allocated first; all underwriting revenue belongs to junior capital.
    pub fn quote(net_repayment: u64, surplus_cap: u64) -> Self {
        let total = (u128::from(net_repayment) * u128::from(LIQUIDATION_FEE_BPS) / u128::from(BPS_DENOMINATOR))
            .min(u128::from(surplus_cap)) as u64;
        let protocol =
            (u128::from(total) * u128::from(LIQUIDATION_PROTOCOL_SHARE_BPS) / u128::from(BPS_DENOMINATOR)) as u64;
        Self {
            total,
            protocol,
            junior: total - protocol,
        }
    }
}

/// A completed debt leg. `sale_recovery` is net of the caller reward and actual
/// transfer costs. Unpaid borrower/leverage interest is canceled.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationLossAllocation {
    pub principal_repaid: u64,
    pub interest_paid: u64,
    pub principal_written_off: u64,
    pub interest_canceled: u64,
    pub surplus: u64,
}

impl LiquidationLossAllocation {
    /// Recovery pays principal first. The remaining funded credit loss is
    /// allocated to junior and then senior by the market tranche ledger.
    /// Unpaid borrower interest is canceled, never converted to principal loss.
    pub fn quote(principal: u64, debt: u64, sale_recovery: u64) -> Result<Self> {
        require_gte!(debt, principal, ErrorCode::BrokenInvariant);
        let principal_repaid = principal.min(sale_recovery);
        let interest = debt - principal;
        let interest_paid = interest.min(sale_recovery.saturating_sub(principal));
        Ok(Self {
            principal_repaid,
            interest_paid,
            principal_written_off: principal - principal_repaid,
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
    fn fixed_liquidation_fee_funds_junior_and_conserves_recovered_surplus() {
        for repayment in [0, 99, 100, 499, 500, u64::MAX] {
            for surplus in [0, 1, 55, u64::MAX] {
                let fee = LiquidationFeeAllocation::quote(repayment, surplus);
                assert!(fee.total <= surplus);
                assert_eq!(
                    fee.total,
                    ((u128::from(repayment) * 300 / 10_000).min(u128::from(surplus))) as u64
                );
                assert_eq!(fee.protocol, fee.total / 5);
                assert_eq!(fee.protocol + fee.junior, fee.total);
            }
        }
        assert_eq!(
            LiquidationFeeAllocation::quote(100_000, u64::MAX),
            LiquidationFeeAllocation {
                total: 3_000,
                protocol: 600,
                junior: 2_400
            }
        );
    }

    #[test]
    fn recovery_pays_principal_and_does_not_socialize_unpaid_interest() {
        let loss = LiquidationLossAllocation::quote(900, 950, 850).unwrap();
        assert_eq!(loss.principal_repaid, 850);
        assert_eq!(loss.principal_written_off, 50);
        assert_eq!(loss.interest_paid, 0);
        assert_eq!(loss.interest_canceled, 50);
        let partial_interest = LiquidationLossAllocation::quote(900, 950, 920).unwrap();
        assert_eq!(partial_interest.principal_written_off, 0);
        assert_eq!(partial_interest.interest_paid, 20);
        assert_eq!(partial_interest.interest_canceled, 30);
        let solvent = LiquidationLossAllocation::quote(900, 950, 1_000).unwrap();
        assert_eq!(
            (solvent.principal_repaid, solvent.interest_paid, solvent.surplus),
            (900, 50, 50)
        );
    }
}
