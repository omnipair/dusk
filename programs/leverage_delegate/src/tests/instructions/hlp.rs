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

#[test]
fn a_rotated_hlp_mint_is_a_retired_cohort() {
    let mut market = Market::default();
    market.base_side.hlp_mint = Pubkey::new_unique();
    market.quote_side.hlp_mint = Pubkey::new_unique();
    let stored = market.base_side.hlp_mint;
    assert!(current_hlp_asset(&market, stored).unwrap() == MarketAsset::Base);
    assert!(current_hlp_asset(&market, market.quote_side.hlp_mint).unwrap() == MarketAsset::Quote);

    // rotate_hlp_cohort installs a new mint for the side.
    market.base_side.hlp_mint = Pubkey::new_unique();
    assert_eq!(
        current_hlp_asset(&market, stored).unwrap_err(),
        error!(LeverageDelegateError::HlpCohortRetired)
    );
}
