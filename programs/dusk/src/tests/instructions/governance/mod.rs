use super::*;

#[test]
fn eligible_supply_adds_all_governance_locks_back_before_excluding_hlp_vaults() {
    let mut market = Market::default();
    let before_lock = direct_ylp_eligible_supply(&market, 1_000, 125, 75).unwrap();

    market.governance_locked_ylp = 300;
    let after_lock = direct_ylp_eligible_supply(&market, 700, 125, 75).unwrap();

    assert_eq!(before_lock, 800);
    assert_eq!(after_lock, before_lock);
}

#[test]
fn eligible_supply_rejects_vault_balances_outside_the_mint_supply_partition() {
    let market = Market::default();
    assert!(direct_ylp_eligible_supply(&market, 100, 60, 41).is_err());
}
