use super::*;

fn protection_order() -> ProtectionOrder {
    ProtectionOrder {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position: Pubkey::new_unique(),
        position_owner: Pubkey::new_unique(),
        lp_mint: Pubkey::new_unique(),
        custody_lp_account: Pubkey::new_unique(),
        order_id: 1,
        action: 0,
        debt_asset: 1,
        active: true,
        remaining_lp: 3_000,
        max_lp_per_execution: 1_500,
        max_payment_per_execution: 1_000,
        trigger_health_bps: 1_000_000,
        target_health_bps: 1_000_001,
        min_payment_per_lp_nad: 100_000_000,
        keeper_fee_bps: 100,
        expires_at: i64::MAX,
        bump: 255,
    }
}

#[test]
fn capped_protection_may_stop_short_of_the_target() {
    let order = protection_order();
    let (before, improved) = (30_000, 35_000);

    // Below target, neither the payment nor the LP burn reaches its cap.
    assert!(!order.accepts_health(before, improved, 1_000, 999));
    // The whole payment allowance, or the whole LP allowance, may stop short.
    assert!(order.accepts_health(before, improved, 1_000, 1_000));
    assert!(order.accepts_health(before, improved, 1_500, 600));
    // Reaching the target needs no cap, and a cleared debt reads u64::MAX.
    assert!(order.accepts_health(before, 1_000_001, 1, 1));
    assert!(order.accepts_health(before, u64::MAX, 1, 1));
    // A capped execution must still improve health.
    assert!(!order.accepts_health(before, before, 1_500, 1_000));
    assert!(!order.accepts_health(before, before - 1, 1_500, 1_000));
}

#[test]
fn a_budget_below_the_lp_cap_caps_the_burn() {
    let mut order = protection_order();
    order.remaining_lp = 500;
    // The last 500 LP is the most this execution may burn.
    assert!(order.accepts_health(30_000, 31_000, 500, 300));
    assert!(!order.accepts_health(30_000, 31_000, 499, 300));
    // With budget to spare, the LP cap is the per-execution maximum.
    order.remaining_lp = 3_000;
    assert!(!order.accepts_health(30_000, 31_000, 500, 300));
}
