use super::*;

/// A market lending `principal` of each asset from 1,000,000 of liquidity,
/// with the given markup and operator share.
fn operator_market(markup: u16, operator_share: u16, principal: u64) -> Market {
    let mut market = Market::current_layout();
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.protocol_interest_markup_bps = markup;
    market.operator.interest_share_bps = operator_share;
    market.debt.fixed_base_shares = u128::from(principal);
    market.debt.fixed_base_principal = principal;
    market.debt.fixed_quote_shares = u128::from(principal);
    market.debt.fixed_quote_principal = principal;
    for side in [&mut market.base_side, &mut market.quote_side] {
        side.shares.srlp_supply = 1_000_000;
        side.reserves.live_reserve = 1_000_000;
        side.reserves.cash_reserve = 1_000_000 - principal;
    }
    market
}

/// Advance the base borrow index by `bps` and recognize the accrual.
fn accrue_base(market: &mut Market, bps: u128) -> InterestCredit {
    let before = market.debt.fixed_base_debt().unwrap();
    market.debt.base_borrow_index_nad += market.debt.base_borrow_index_nad * bps / 10_000;
    let accrued = market.debt.fixed_base_debt().unwrap() - before;
    market.base_side.reserves.live_reserve += accrued;
    market.recognize_accrued_interest(MarketAsset::Base).unwrap()
}

#[test]
fn operator_and_protocol_split_carries_conserve_each_accrual() {
    let mut market = operator_market(2_000, 3_000, 100_000);
    let (mut gross, mut protocol, mut operator) = (0_u64, 0_u64, 0_u64);
    for bps in [1, 3, 7, 11, 13, 101, 997] {
        let credit = accrue_base(&mut market, bps);
        assert_eq!(credit.netted, 0);
        gross += credit.protocol + credit.operator + credit.lp;
        protocol += credit.protocol;
        operator += credit.operator;
    }
    let ledger = market.base_side.fees.ledger;
    // The markup is added on top of the base rate: protocol = gross × m/(1+m).
    let exact_protocol = u128::from(gross) * 2_000 / 12_000;
    assert!(u128::from(protocol) <= exact_protocol && u128::from(protocol) + 1 >= exact_protocol);
    let exact_operator = u128::from(gross - protocol) * 3 / 10;
    assert!(u128::from(operator).abs_diff(exact_operator) <= 1);
    assert_eq!(ledger.protocol_unpaid, u128::from(protocol));
    assert_eq!(ledger.operator_claimable, u128::from(operator));
    assert_eq!(ledger.credited, u128::from(gross - protocol));
    assert_eq!(ledger.public_recognized, u128::from(gross));
    market.assert_interest_ledger(MarketAsset::Base).unwrap();
}

#[test]
fn recipient_handoff_archives_the_credited_share_and_new_credit_follows_the_new_recipient() {
    let mut market = operator_market(0, 5_000, 100_000);
    accrue_base(&mut market, 1_000);
    let outgoing = market.base_side.fees.ledger.operator_claimable;
    assert_eq!(outgoing, 5_000);
    // The handoff seals the outgoing recipient's credited share in its
    // archive; it stays inside `credited` until claimed.
    let mut archive = [0_u128; 2];
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let ledger = &mut market.side_mut(asset).fees.ledger;
        archive[asset.code() as usize] = ledger.operator_claimable;
        ledger.operator_claimable = 0;
    }
    let credited = market.base_side.fees.ledger.credited;
    accrue_base(&mut market, 500);
    let incoming = market.base_side.fees.ledger.operator_claimable;
    assert_eq!(incoming, 2_750);
    assert_eq!(archive[0], outgoing);
    assert_eq!(market.base_side.fees.ledger.credited, credited + 5_500);
    // Both recipients claim from reserve cash; neither touches the other.
    let curve = market.curve_reserve(MarketAsset::Base).unwrap();
    let paid = market.base_side.pay_interest(archive[0] as u64).unwrap();
    archive[0] -= u128::from(paid);
    assert_eq!((paid, archive[0]), (5_000, 0));
    let paid = market.base_side.pay_interest(incoming as u64).unwrap();
    market.base_side.fees.ledger.operator_claimable -= u128::from(paid);
    assert_eq!(paid, 2_750);
    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), curve);
    market.assert_interest_ledger(MarketAsset::Base).unwrap();
}

#[test]
fn operator_claim_pays_what_cash_allows_and_keeps_the_rest_claimable() {
    let mut market = operator_market(0, 10_000, 990_000);
    accrue_base(&mut market, 2_000);
    let ledger = market.base_side.fees.ledger;
    assert_eq!(ledger.operator_claimable, 198_000);
    assert_eq!(market.base_side.reserves.cash_reserve, 10_000);
    let curve = market.curve_reserve(MarketAsset::Base).unwrap();
    let paid = market.base_side.pay_interest(198_000).unwrap();
    market.base_side.fees.ledger.operator_claimable -= u128::from(paid);
    assert_eq!(paid, 10_000);
    assert_eq!(market.base_side.reserves.cash_reserve, 0);
    assert_eq!(market.base_side.fees.ledger.operator_claimable, 188_000);
    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), curve);
    assert_eq!(market.base_side.pay_interest(1).unwrap(), 0);
    market.assert_interest_ledger(MarketAsset::Base).unwrap();
}

#[test]
fn a_default_keeps_the_operator_credit_and_the_pool_absorbs_it() {
    let mut market = operator_market(1_000, 3_000, 100_000);
    accrue_base(&mut market, 1_000);
    let ledger = market.base_side.fees.ledger;
    let curve = market.curve_reserve(MarketAsset::Base).unwrap();
    let debt = u64::try_from(market.debt.fixed_base_debt().unwrap()).unwrap();
    market.debt.fixed_base_shares = 0;
    market.debt.fixed_base_principal = 0;
    // Nothing recovered: principal and every unpaid atom are written off.
    let (protocol_paid, loss) = market.settle_public_debt(MarketAsset::Base, 0, 0, debt).unwrap();
    let after = market.base_side.fees.ledger;
    assert_eq!(protocol_paid, 0);
    assert_eq!(after.operator_claimable, ledger.operator_claimable);
    assert_eq!(after.credited, ledger.credited);
    assert_eq!(after.protocol_unpaid, 0);
    // The pool covers principal and the LP and operator credit; the
    // protocol's unpaid share is dropped.
    assert_eq!(u128::from(loss), u128::from(debt) - ledger.protocol_unpaid);
    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), curve - loss);
    market.assert_interest_ledger(MarketAsset::Base).unwrap();
}
