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

#[test]
fn governance_locks_keep_unpaid_income_without_duplicating_virtual_cash_yield() {
    let mut market = Market::default();
    market.base_side.asset_mint = Pubkey::new_unique();
    market.quote_side.asset_mint = Pubkey::new_unique();
    market.add_liquidity(10_000, 10_000).unwrap();
    market.tranches.enter(2_000 - market.tranches.junior_reserve_shares, 10_000).unwrap();
    market.synchronize_tranche_income();
    let mut base = YieldAccount { asset_mint: market.base_side.asset_mint, ..Default::default() };
    let mut quote = YieldAccount { asset_mint: market.quote_side.asset_mint, ..Default::default() };
    let indexes = GovernanceYieldIndexes { base_swap_fee_q64: 0, base_interest_q64: 0, quote_swap_fee_q64: 0, quote_interest_q64: 0 };
    checkpoint_supporter_yield(&market, &mut base, &mut quote, 8_000, indexes).unwrap();
    adjust_supporter_income_balance(&mut base, &mut quote, 5_000, true).unwrap();
    market.issue_income_receivable(crate::state::MarketAsset::Base, false, 1_000).unwrap();
    checkpoint_supporter_yield(&market, &mut base, &mut quote, 3_000, indexes).unwrap();
    assert_eq!(base.income[0].units_q64, 500_u128 << 64);
    // Cash growth for the locked 5,000 is already tracked by VirtualYieldLedger.
    base.accrue(3_000, 1_u128 << 64, 0).unwrap();
    assert_eq!(base.accrued_swap_fee_amount, 3_000);
    adjust_supporter_income_balance(&mut base, &mut quote, 5_000, false).unwrap();
    market.clear_income_receivable(crate::state::MarketAsset::Base, false, 1_000, 1_000).unwrap();
    market.record_receivable_income_credit(crate::state::MarketAsset::Base, false, 1_000, 0, Default::default(), 0).unwrap();
    market.checkpoint_holder_income(&mut base, 8_000, None).unwrap();
    assert_eq!(base.accrued_interest_amount, 500);
    assert_eq!(base.escrow_balance, 0);
    assert!(adjust_supporter_income_balance(&mut base, &mut quote, 1, false).is_err());
}
