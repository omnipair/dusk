use super::*;
use proptest::prelude::*;

fn nad(x: u128) -> u128 {
    x * NAD as u128
}

fn apply_signed(value: u128, delta: i128) -> u128 {
    if delta >= 0 {
        value.checked_add(delta as u128).unwrap()
    } else {
        value.checked_sub(delta.unsigned_abs()).unwrap()
    }
}

#[test]
fn target_hlp_is_neutral_when_opposite_inventory_equals_debt() {
    let values = HlpInventoryValuesNad {
        target_inventory_value_nad: nad(40),
        opposite_inventory_value_nad: nad(60),
        debt_value_nad: nad(60),
    };
    assert_eq!(hlp_opposite_exposure_nad(values).unwrap(), 0);
    assert_eq!(ideal_hlp_rebalance_nad(values).unwrap().total_liquidity_value_nad, 0);
}

#[test]
fn off_center_concentrated_inventory_leverages_up_proportionally() {
    // T=40, O=60, D=50. The old C-2D expression incorrectly returns zero.
    // The real opposite exposure is +10, so L = 10*100/40 = +25.
    let values = HlpInventoryValuesNad {
        target_inventory_value_nad: nad(40),
        opposite_inventory_value_nad: nad(60),
        debt_value_nad: nad(50),
    };
    let adjustment = ideal_hlp_rebalance_nad(values).unwrap();
    assert_eq!(hlp_opposite_exposure_nad(values).unwrap(), nad(10) as i128);
    assert_eq!(adjustment.total_liquidity_value_nad, nad(25) as i128);
    assert_eq!(adjustment.target_inventory_value_delta_nad, nad(10) as i128);
    assert_eq!(adjustment.opposite_inventory_value_delta_nad, nad(15) as i128);
    assert_eq!(adjustment.debt_value_delta_nad, nad(25) as i128);
}

#[test]
fn off_center_concentrated_inventory_deleverages_proportionally() {
    // T=75, O=25, D=40. L = -15*100/75 = -20. Removing 15/5 of
    // target/opposite inventory and repaying 20 leaves O'=D'=20.
    let values = HlpInventoryValuesNad {
        target_inventory_value_nad: nad(75),
        opposite_inventory_value_nad: nad(25),
        debt_value_nad: nad(40),
    };
    let adjustment = ideal_hlp_rebalance_nad(values).unwrap();
    assert_eq!(adjustment.total_liquidity_value_nad, -(nad(20) as i128));
    assert_eq!(adjustment.target_inventory_value_delta_nad, -(nad(15) as i128));
    assert_eq!(adjustment.opposite_inventory_value_delta_nad, -(nad(5) as i128));
    assert_eq!(adjustment.debt_value_delta_nad, -(nad(20) as i128));

    let opposite_after = apply_signed(
        values.opposite_inventory_value_nad,
        adjustment.opposite_inventory_value_delta_nad,
    );
    let debt_after = apply_signed(values.debt_value_nad, adjustment.debt_value_delta_nad);
    assert_eq!(opposite_after, debt_after);
}

#[test]
fn proportional_allocation_uses_actual_values_not_half() {
    let values = HlpInventoryValuesNad {
        target_inventory_value_nad: nad(30),
        opposite_inventory_value_nad: nad(70),
        debt_value_nad: 0,
    };
    let adjustment = allocate_hlp_proportional_adjustment_nad(values, nad(10) as i128).unwrap();
    assert_eq!(adjustment.target_inventory_value_delta_nad, nad(3) as i128);
    assert_eq!(adjustment.opposite_inventory_value_delta_nad, nad(7) as i128);
}

#[test]
fn zero_and_missing_target_inventory_edges_are_explicit() {
    let empty = HlpInventoryValuesNad::default();
    assert_eq!(ideal_hlp_rebalance_nad(empty).unwrap(), Default::default());
    assert_eq!(
        allocate_hlp_proportional_adjustment_nad(empty, 0).unwrap(),
        Default::default()
    );

    let no_target = HlpInventoryValuesNad {
        target_inventory_value_nad: 0,
        opposite_inventory_value_nad: nad(10),
        debt_value_nad: nad(5),
    };
    assert!(ideal_hlp_rebalance_nad(no_target).is_err());
    assert!(allocate_hlp_proportional_adjustment_nad(no_target, nad(1) as i128).is_ok());
}

#[test]
fn unrepresentable_signed_exposure_is_rejected() {
    let values = HlpInventoryValuesNad {
        target_inventory_value_nad: 1,
        opposite_inventory_value_nad: u128::MAX,
        debt_value_nad: 0,
    };
    assert!(hlp_opposite_exposure_nad(values).is_err());
    assert!(ideal_hlp_rebalance_nad(values).is_err());
}

#[test]
fn native_mul_div_handles_wide_product_when_quotient_fits() {
    assert_eq!(mul_div_u128(u128::MAX, 2, u128::MAX).unwrap(), 2);
    assert_eq!(mul_div_u128(1_u128 << 127, 3, 2).unwrap(), 3_u128 << 126);
    assert!(mul_div_u128(u128::MAX, u128::MAX, 1).is_err());
}

#[test]
fn ratio_comparison_handles_overflowing_cross_products_exactly() {
    assert!(ratio_lte_full_width(u128::MAX - 1, u128::MAX, u128::MAX, u128::MAX).unwrap());
    assert!(!ratio_lte_full_width(u128::MAX, u128::MAX - 1, u128::MAX, u128::MAX).unwrap());
    assert!(ratio_lte_full_width(u128::MAX, u128::MAX, u128::MAX, u128::MAX).unwrap());
}

proptest! {
    #[test]
    fn cpmm_equal_value_parity_with_collateral_minus_twice_debt(
        inventory in 1_u64..=1_000_000_000_u64,
        debt in 0_u64..=2_000_000_000_u64,
    ) {
        let values = HlpInventoryValuesNad {
            target_inventory_value_nad: inventory as u128,
            opposite_inventory_value_nad: inventory as u128,
            debt_value_nad: debt as u128,
        };
        let actual = ideal_hlp_rebalance_nad(values).unwrap().total_liquidity_value_nad;
        let legacy = (inventory as i128)
            .checked_mul(2)
            .unwrap()
            .checked_sub((debt as i128).checked_mul(2).unwrap())
            .unwrap();
        prop_assert_eq!(actual, legacy);
    }

    #[test]
    fn allocated_legs_sum_exactly_and_preserve_inventory_weights_with_rounding(
        target in 1_u64..=1_000_000_000_u64,
        opposite in 0_u64..=1_000_000_000_u64,
        total in -1_000_000_000_i64..=1_000_000_000_i64,
    ) {
        let values = HlpInventoryValuesNad {
            target_inventory_value_nad: target as u128,
            opposite_inventory_value_nad: opposite as u128,
            debt_value_nad: 0,
        };
        let adjustment =
            allocate_hlp_proportional_adjustment_nad(values, total as i128).unwrap();
        prop_assert_eq!(
            adjustment
                .target_inventory_value_delta_nad
                .checked_add(adjustment.opposite_inventory_value_delta_nad)
                .unwrap(),
            total as i128
        );
        prop_assert_eq!(adjustment.debt_value_delta_nad, total as i128);

        let total_value = target as u128 + opposite as u128;
        let target_magnitude = adjustment.target_inventory_value_delta_nad.unsigned_abs();
        let total_magnitude = (total as i128).unsigned_abs();
        let exact_numerator = total_magnitude * target as u128;
        prop_assert_eq!(target_magnitude, exact_numerator / total_value);
    }

    #[test]
    fn ideal_adjustment_leaves_at_most_one_raw_nad_unit_of_exposure(
        target in 1_u64..=1_000_000_000_u64,
        opposite in 0_u64..=1_000_000_000_u64,
        debt_fraction_bps in 0_u16..=10_000_u16,
    ) {
        let collateral = target as u128 + opposite as u128;
        let debt = collateral * debt_fraction_bps as u128 / 10_000;
        let values = HlpInventoryValuesNad {
            target_inventory_value_nad: target as u128,
            opposite_inventory_value_nad: opposite as u128,
            debt_value_nad: debt,
        };
        let adjustment = ideal_hlp_rebalance_nad(values).unwrap();
        let target_after = apply_signed(
            values.target_inventory_value_nad,
            adjustment.target_inventory_value_delta_nad,
        );
        let opposite_after = apply_signed(
            values.opposite_inventory_value_nad,
            adjustment.opposite_inventory_value_delta_nad,
        );
        let debt_after = apply_signed(values.debt_value_nad, adjustment.debt_value_delta_nad);
        let residual = opposite_after.abs_diff(debt_after);
        let collateral_after =
            apply_signed(collateral, adjustment.total_liquidity_value_nad);

        prop_assert!(residual <= 1);
        prop_assert_eq!(
            target_after.checked_add(opposite_after).unwrap(),
            collateral_after
        );
    }
}

#[test]
fn isqrt_matches_floor_sqrt() {
    assert_eq!(isqrt(0), 0);
    assert_eq!(isqrt(1), 1);
    assert_eq!(isqrt(4), 2);
    assert_eq!(isqrt(8), 2);
    assert_eq!(isqrt(9), 3);
    assert_eq!(isqrt(1_000_000), 1_000);
    let big = (u64::MAX as u128) * (u64::MAX as u128);
    assert_eq!(isqrt(big), u64::MAX as u128);
}

#[test]
fn sqrt_ratio_of_1_44_is_1_2() {
    // r = 1.44 -> sqrt = 1.2.
    let r = nad(144) / 100;
    assert_eq!(sqrt_ratio_nad(r).unwrap(), nad(12) / 10);
}

#[test]
fn tracking_loss_matches_closed_form() {
    // E0 = 100, r = 1.44 -> loss = 100 * (1.2 - 1)^2 = 100 * 0.04 = 4.
    let loss = tracking_loss_nad(nad(100), nad(144) / 100).unwrap();
    assert_eq!(loss, nad(4));
}

#[test]
fn tracking_loss_is_zero_at_unit_ratio() {
    assert_eq!(tracking_loss_nad(nad(100), nad(1)).unwrap(), 0);
}

#[test]
fn tracking_loss_matches_downside_closed_form() {
    // E0 = 100, r = 0.64 -> loss = 100 * (0.8 - 1)^2 = 4.
    let loss = tracking_loss_nad(nad(100), nad(64) / 100).unwrap();
    assert_eq!(loss, nad(4));
}

#[test]
fn closed_form_pre_adjustment_upside() {
    // E0 = 100, r = 1.44 -> Δpre = 100 * (1.2 - 1) = 20, lever up.
    let (amount, lever_up) = closed_form_pre_adjustment_nad(nad(100), nad(144) / 100).unwrap();
    assert_eq!(amount, nad(20));
    assert!(lever_up);
}

#[test]
fn closed_form_pre_adjustment_downside_is_deleverage() {
    // r = 0.64 -> sqrt = 0.8 -> |Δpre| = 100 * 0.2 = 20, deleverage.
    let (amount, lever_up) = closed_form_pre_adjustment_nad(nad(100), nad(64) / 100).unwrap();
    assert_eq!(amount, nad(20));
    assert!(!lever_up);
}
