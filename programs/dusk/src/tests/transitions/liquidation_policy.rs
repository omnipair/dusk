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
