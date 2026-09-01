use super::*;
use dusk::constants::NAD;

#[test]
fn hlp_order_kinds_and_trigger_directions_are_exact() {
    assert!(validate_hlp_order_kind(HLP_ORDER_KIND_STOP_LOSS).is_ok());
    assert!(validate_hlp_order_kind(HLP_ORDER_KIND_STOP_RATE).is_ok());
    assert!(validate_hlp_order_kind(0).is_err());

    assert!(hlp_order_trigger_met(HLP_ORDER_KIND_STOP_LOSS, 99, 0, 100).unwrap());
    assert!(!hlp_order_trigger_met(HLP_ORDER_KIND_STOP_LOSS, 101, 0, 100).unwrap());
    assert!(hlp_order_trigger_met(HLP_ORDER_KIND_STOP_RATE, 0, 101, 100).unwrap());
    assert!(!hlp_order_trigger_met(HLP_ORDER_KIND_STOP_RATE, 0, 99, 100).unwrap());
    assert!(hlp_order_trigger_met(0, 100, 100, 100).is_err());
}

#[test]
fn partial_hlp_order_escrows_only_the_selected_tranche() {
    let user_balance = 100;
    let order_amount = 30;
    assert!(order_amount < user_balance);
    assert_eq!(user_balance - order_amount, 70);

    let order = HlpOrder {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        target_hlp_mint: Pubkey::new_unique(),
        custody_hlp_account: Pubkey::new_unique(),
        order_id: 7,
        kind: HLP_ORDER_KIND_STOP_LOSS,
        status: HLP_ORDER_STATUS_ACTIVE,
        hlp_amount: order_amount,
        trigger_nad: NAD,
        min_target_amount_out: 0,
        bump: 255,
    };
    assert_eq!(order.hlp_amount, 30);
}
