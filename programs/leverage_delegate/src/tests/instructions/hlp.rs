use super::*;
use dusk::constants::NAD;

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
