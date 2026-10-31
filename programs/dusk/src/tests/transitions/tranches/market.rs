use super::*;

fn market() -> Market {
    let mut market = Market::default();
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.amm.launch_reference_price_nad = NAD;
    market.add_liquidity(100_000, 100_000).unwrap();
    let extra = 20_000 - market.tranches.junior_reserve_shares;
    market.tranches.enter(extra, 100_000).unwrap();
    market.synchronize_tranche_income();
    for side in [&mut market.base_side, &mut market.quote_side] { side.reserves.cash_reserve = 20_000; }
    market.debt.fixed_base_shares = 80_000;
    market.debt.fixed_quote_shares = 80_000;
    market.debt.fixed_base_principal = 80_000;
    market.debt.fixed_quote_principal = 80_000;
    market
}

#[test]
fn one_shared_junior_budget_produces_reference_signal_on_both_sides() {
    let market = market();
    assert_eq!(market.tranche_coverage_signals().unwrap(), [Some(50_000_000); 2]);
    assert_eq!(market.funded_reserve_value().unwrap(), 200_000_000_000_000);
}

#[test]
fn senior_exit_changes_cash_signal_without_changing_junior_value() {
    let mut market = market();
    let before = market.tranches.junior_value(market.funded_reserve_value().unwrap(), 100_000).unwrap();
    market.base_side.reserves.cash_reserve -= 10_000;
    market.base_side.reserves.live_reserve -= 10_000;
    market.quote_side.reserves.cash_reserve -= 10_000;
    market.quote_side.reserves.live_reserve -= 10_000;
    market.base_side.shares.ylp_supply -= 10_000;
    market.quote_side.shares.ylp_supply -= 10_000;
    let after = market.tranches.junior_value(market.funded_reserve_value().unwrap(), 90_000).unwrap();
    assert_eq!(before, after);
    let signals = market.tranche_coverage_signals().unwrap();
    assert!(signals[0].unwrap() < 28_000_000 && signals[0].unwrap() > 27_700_000);
    assert_eq!(signals[0], signals[1]);
}

#[test]
fn funded_public_loss_burns_only_junior_common_units_before_senior() {
    let mut market = market();
    market.debt.fixed_base_shares -= 8_000;
    market.debt.fixed_base_principal -= 8_000;
    market.settle_public_debt_reserves(MarketAsset::Base, 0, 0, 8_000, 0).unwrap();
    assert_eq!(market.tranches.junior_reserve_shares, 16_000);
    assert_eq!(market.base_side.shares.ylp_supply, 96_000);
    assert_eq!(market.tranches.senior_reserve_shares(96_000).unwrap(), 80_000);
    assert_eq!(market.funded_reserve_value().unwrap() * 80_000 / 96_000, 160_000_000_000_000);
    // A quote loss consumes the remaining shared budget, not another copy.
    market.debt.fixed_quote_shares -= 40_000;
    market.debt.fixed_quote_principal -= 40_000;
    market.settle_public_debt_reserves(MarketAsset::Quote, 0, 0, 40_000, 0).unwrap();
    assert_eq!(market.tranches.junior_reserve_shares, 0);
    assert_eq!(market.base_side.shares.ylp_supply, 80_000);
    assert_eq!(market.funded_reserve_value().unwrap(), 152_000_000_000_000);
}

#[test]
fn writing_off_unadvanced_interest_does_not_consume_funded_junior() {
    let mut market = market();
    market.debt.fixed_base_shares += 10_000;
    market.base_side.reserves.live_reserve += 10_000;
    assert_eq!(market.public_unpaid_interest(MarketAsset::Base).unwrap(), 10_000);
    let before = market.tranches;
    market.debt.fixed_base_shares -= 10_000;
    market.settle_public_debt_reserves(MarketAsset::Base, 10_000, 0, 10_000, 0).unwrap();
    assert_eq!(market.tranches, before);
}

#[test]
fn cash_limits_allow_bootstrap_borrowing_without_junior() {
    let mut market = market();
    market.tranches.junior_reserve_shares = 0;
    market.synchronize_tranche_income();
    market.debt.fixed_base_shares = 0;
    market.debt.fixed_base_principal = 0;
    market.base_side.reserves.cash_reserve = 100_000;
    assert_eq!(market.borrow_cash_headroom(MarketAsset::Base).unwrap(), 80_000);
}

#[test]
fn collected_revenue_is_split_and_seed_keeps_its_own_earnings() {
    let mut market = market();
    let senior = 80_000;
    let junior = 20_000;
    let old_seed = market.tranches.permanent_receipts;
    market.base_side.record_interest_credit_with_supply(10_000, 0, Default::default(), 0, senior + junior).unwrap();
    let fees = &market.base_side.fees;
    assert_eq!(fees.interest_liability, 10_000);
    assert_eq!(fees.interest_growth_index_q64 * senior as u128 >> 64, 5_000);
    assert_eq!(fees.junior.interest_growth_q64 * junior as u128 >> 64, 5_000);
    market.checkpoint_junior_seed().unwrap();
    assert_eq!(market.base_side.fees.junior.seed_interest_amount, 5_000 * old_seed / junior);
    // An exit fee changes seed ownership only after its old income checkpoint.
    market.tranches.pay_seed_fee(100).unwrap();
    market.synchronize_tranche_income();
    let claim = market.base_side.fees.junior.seed_interest_amount;
    market.checkpoint_junior_seed().unwrap();
    assert_eq!(market.base_side.fees.junior.seed_interest_amount, claim);
}

#[test]
fn permanent_seed_reinvestment_does_not_mint_senior_claims() {
    let mut market = market();
    let senior_before = market.tranches.senior_reserve_shares(market.base_side.shares.ylp_supply).unwrap();
    let seed_before = market.tranches.permanent_receipts;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let side = market.side_mut(asset);
        side.fees.junior.seed_interest_amount = 1_000;
        side.fees.interest_liability = 1_000;
        side.fees.interest_vault_balance = 1_000;
        market.fund_junior_seed(asset, 1_000, 990).unwrap();
    }
    let funded = market.compound_junior_seed().unwrap();
    assert_eq!(funded.ylp_amount, 990);
    assert_eq!(market.tranches.senior_reserve_shares(funded.ylp_supply).unwrap(), senior_before);
    assert_eq!(market.tranches.permanent_receipts, seed_before + 990);
    assert_eq!(market.base_side.fees.swap_fee_custody_balance, 0);
    assert_eq!(market.base_side.reserves.cash_reserve, 20_990);
    market.assert_market_invariants().unwrap();
}

#[test]
fn seed_reinvestment_requires_archival_after_a_complete_wipe() {
    let mut market = market();
    market.tranches.junior_reserve_shares = 0;
    market.synchronize_tranche_income();
    for side in [&mut market.base_side, &mut market.quote_side] {
        side.fees.junior.seed_swap_amount = 1_000;
        side.fees.swap_fee_liability = 1_000;
        side.fees.swap_fee_custody_balance = 1_000;
    }
    assert!(market.compound_junior_seed().is_err());
    market.tranches.retire_wiped_generation().unwrap();
    market.synchronize_tranche_income();
    let funded = market.compound_junior_seed().unwrap();
    assert_eq!(market.tranches.junior_reserve_shares, funded.ylp_amount);
    assert_eq!(market.tranches.permanent_receipts, market.tranches.junior_receipt_supply);
    assert_eq!(market.tranches.generation, 1);
}

#[test]
fn senior_exit_keeps_earned_income_without_withdrawing_it_twice() {
    let mut market = market();
    market.base_side.asset_mint = Pubkey::new_unique();
    market.quote_side.asset_mint = Pubkey::new_unique();
    let mut departing = crate::state::YieldAccount {
        asset_mint: market.base_side.asset_mint,
        token_kind: crate::state::YieldTokenKind::Ylp.code(),
        ..Default::default()
    };
    market.checkpoint_holder_income(&mut departing, 0, None).unwrap();
    market.debt.fixed_base_shares += 10_000;
    market.base_side.reserves.live_reserve += 10_000;
    market.sync_income_receivable(MarketAsset::Base, false, 0).unwrap();
    market.checkpoint_holder_income(&mut departing, 10_000, None).unwrap();
    let redeemed = market.remove_liquidity(10_000).unwrap();
    assert_eq!((redeemed.base_amount_out, redeemed.quote_amount_out), (10_000, 10_000));
    // The depositor buys funded capital, not this owner's historical coupon.
    let entry = market.add_liquidity(10_000, 10_000).unwrap();
    assert_eq!(entry.ylp_amount, 10_000);
    market.debt.fixed_base_shares -= 10_000;
    market.settle_public_debt_reserves(MarketAsset::Base, 10_000, 10_000, 10_000, 0).unwrap();
    market.record_receivable_income_credit(MarketAsset::Base, false, 10_000, 0, Default::default(), 0).unwrap();
    market.checkpoint_holder_income(&mut departing, 0, None).unwrap();
    assert_eq!(departing.accrued_interest_amount, 625);
}

#[test]
fn newly_created_retired_holder_never_inherits_new_generation_income() {
    let mut market = market();
    market.base_side.asset_mint = Pubkey::new_unique();
    market.quote_side.asset_mint = Pubkey::new_unique();
    market.issue_income_receivable(MarketAsset::Base, false, 1_000).unwrap();
    let mut ended = [crate::state::IncomeExposureEnd::default(); 4];
    for (i, end) in ended.iter_mut().enumerate() {
        *end = crate::state::IncomeExposureEnd { ended: true, epoch: market.income.get().unwrap().books[i].epoch, growth: market.income.get().unwrap().books[i].growth[1] };
    }
    market.issue_income_receivable(MarketAsset::Base, false, 1_000).unwrap();
    let mut recipient = crate::state::YieldAccount {
        asset_mint: market.base_side.asset_mint,
        token_kind: crate::state::YieldTokenKind::Junior.code(),
        ..Default::default()
    };
    market.checkpoint_holder_income(&mut recipient, 0, Some(ended)).unwrap();
    market.clear_income_receivable(MarketAsset::Base, false, 2_000, 2_000).unwrap();
    market.record_receivable_income_credit(MarketAsset::Base, false, 2_000, 0, Default::default(), 0).unwrap();
    market.checkpoint_holder_income(&mut recipient, 20_000, Some(ended)).unwrap();
    assert_eq!(recipient.accrued_interest_amount, 0);
    assert_eq!(recipient.income[0].units_q64, 0);
}

#[test]
fn native_compounding_allocates_half_to_each_tranche_including_empty_seed() {
    use crate::transitions::amm::SwapRequest;
    const UNIT: u64 = 1_000_000;
    for amplification in [1, 4] {
        for junior_bps in [0, 2_000, 7_000] {
            for with_hlp in [false, true] {
                for asset in [MarketAsset::Base, MarketAsset::Quote] {
                    let mut initial = Market::default();
                    initial.base_side.asset_decimals = 6;
                    initial.quote_side.asset_decimals = 6;
                    initial.debt.base_borrow_index_nad = NAD as u128;
                    initial.debt.quote_borrow_index_nad = NAD as u128;
                    initial.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
                    if with_hlp {
                        initial.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
                        initial.deposit_single_sided(MarketAsset::Quote, 30_000 * UNIT, 1).unwrap();
                    }
                    let total = initial.base_side.shares.ylp_supply;
                    if junior_bps == 0 {
                        initial.tranches.junior_reserve_shares = 0;
                    } else {
                        let extra = total * junior_bps / 10_000 - initial.tranches.junior_reserve_shares;
                        initial.tranches.enter(extra, total).unwrap();
                    }
                    initial.synchronize_tranche_income();
                    initial.config.swap_fee_bps = 100;
                    initial.config.amm.peak_amplification_nad = amplification * NAD;
                    initial.config.amm.core_half_width_bps = if amplification == 1 { 0 } else { 100 };
                    initial.config.amm.fade_width_bps = if amplification == 1 { 0 } else { 400 };
                    *initial.amm = crate::state::AmmState::default();
                    initial.prepare_amm_for_swap(1).unwrap();
                    let mut baseline = initial.clone();
                    let mut compounded = initial;
                    compounded.config.amm.compounding_fee_bps = 10_000;
                    let request = SwapRequest { current_slot: 1, current_unix_timestamp: 0,
                        asset_in: asset, reserve_credit: 10_000 * UNIT, protocol_fee_bps: 0 };
                    let mut plain = request.prepare(&mut baseline).unwrap();
                    let mut compound = request.prepare(&mut compounded).unwrap();
                    let fee = compound.quote.fee.compounded_fee_debit;
                    let mark = compound.quote.end_price_nad;
                    assert_eq!(plain.quote.amount_out, compound.quote.amount_out);
                    plain.finalize_state(&mut baseline, 1, 0, Default::default()).unwrap();
                    compound.finalize_state(&mut compounded, 1, 0, Default::default()).unwrap();
                    compounded.assert_market_invariants().unwrap();
                    let junior_value = |m: &Market| {
                        let base = m.normalize_amount(m.curve_reserve(MarketAsset::Base).unwrap() as u128, 6).unwrap();
                        let quote = m.normalize_amount(m.curve_reserve(MarketAsset::Quote).unwrap() as u128, 6).unwrap();
                        m.tranches.junior_value(mul_div_u128(base, mark as u128, NAD as u128).unwrap() + quote, m.base_side.shares.ylp_supply).unwrap()
                    };
                    let fee_nad = compounded.normalize_amount((fee / 2) as u128, 6).unwrap();
                    let expected = if asset == MarketAsset::Base { mul_div_u128(fee_nad, mark as u128, NAD as u128).unwrap() } else { fee_nad };
                    if junior_bps == 0 {
                        assert_eq!(compounded.tranches.junior_reserve_shares, 0);
                        assert_eq!(compounded.side(asset).fees.junior.seed_swap_amount, fee / 2);
                    } else {
                        let gain = junior_value(&compounded) - junior_value(&baseline);
                        assert!(gain.abs_diff(expected) <= 100_000,
                            "compound split: amp={amplification} junior={junior_bps} hLP={with_hlp} asset={asset:?} gain={gain} expected={expected}");
                        let old_senior_wallets = baseline.tranches.senior_reserve_shares(baseline.base_side.shares.ylp_supply).unwrap()
                            - baseline.base_hlp_vault.ylp_shares - baseline.quote_hlp_vault.ylp_shares;
                        let new_senior_wallets = compounded.tranches.senior_reserve_shares(compounded.base_side.shares.ylp_supply).unwrap()
                            - compounded.base_hlp_vault.ylp_shares - compounded.quote_hlp_vault.ylp_shares;
                        assert_eq!(old_senior_wallets, new_senior_wallets);
                    }
                }
            }
        }
    }
}
