use super::*;
use crate::{
    constants::{MIN_LIQUIDITY, NAD, YIELD_GROWTH_SCALE_Q64},
    math::{accrue_fee_liability_with_remainder, distribute_growth_q64},
};

/// 10_000 srLP of which the base vault holds 4_000 and the quote vault
/// 2_000, leaving 4_000 ordinary shares.
fn hlp_interest_market(srlp: u64, base_vault: u64, quote_vault: u64) -> Market {
    let mut market = Market::current_layout();
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    for side in [&mut market.base_side, &mut market.quote_side] {
        side.shares.srlp_supply = srlp;
        side.reserves.live_reserve = u128::from(srlp);
        side.reserves.cash_reserve = srlp;
    }
    market.base_hlp_vault.srlp_shares = base_vault;
    market.base_hlp_vault.hlp_supply = base_vault;
    market.quote_hlp_vault.srlp_shares = quote_vault;
    market.quote_hlp_vault.hlp_supply = quote_vault;
    market
}

/// Funding owed by the base vault in quote: `principal` borrowed, `interest`
/// accrued on top.
fn owe_quote_funding(market: &mut Market, principal: u64, interest: u64) {
    market.base_hlp_vault.debt_principal = principal;
    market.base_hlp_vault.debt_shares = u128::from(principal + interest);
}

#[test]
fn funding_interest_credits_ordinary_srlp_and_excludes_both_vaults() {
    let mut market = hlp_interest_market(10_000, 4_000, 2_000);
    owe_quote_funding(&mut market, 1_000, 400);
    let curve_before = market.curve_reserve(MarketAsset::Quote).unwrap();

    let credit = market.recognize_interest(MarketAsset::Quote).unwrap();

    assert_eq!(credit.lp, 400);
    let (growth, carry) = distribute_growth_q64(400, 4_000, 0).unwrap();
    assert_eq!(market.quote_side.fees.interest_growth_index_q64, growth);
    assert_eq!(market.quote_side.fees.interest_growth_remainder_scaled, carry);
    assert_eq!(market.base_hlp_vault.quote_interest_growth_index_q64, 0);
    assert_eq!(market.quote_hlp_vault.quote_interest_growth_index_q64, 0);
    assert_eq!(market.quote_side.fees.ledger.funding_recognized, 400);
    assert_eq!(market.quote_side.fees.ledger.credited, 400);
    assert_eq!(market.curve_reserve(MarketAsset::Quote).unwrap(), curve_before);
    market.assert_interest_ledger(MarketAsset::Quote).unwrap();
}

#[test]
fn public_interest_credits_both_vaults_pro_rata_without_netting() {
    let mut market = hlp_interest_market(10_000, 4_000, 2_000);
    let curve_before = market.curve_reserve(MarketAsset::Quote).unwrap();
    // Accrual adds the interest to the live reserve and credits it at once.
    market.debt.fixed_quote_shares = 2_000;
    market.debt.fixed_quote_principal = 1_000;
    market.quote_side.reserves.live_reserve += 1_000;

    market.recognize_interest(MarketAsset::Quote).unwrap();

    // Senior supply 10_000: base vault 400, quote vault 200, ordinary 400.
    let (ordinary_growth, _) = distribute_growth_q64(400, 4_000, 0).unwrap();
    let (base_vault_growth, _) = distribute_growth_q64(400, 4_000, 0).unwrap();
    let (quote_vault_growth, _) = distribute_growth_q64(200, 2_000, 0).unwrap();
    assert_eq!(market.quote_side.fees.interest_growth_index_q64, ordinary_growth);
    assert_eq!(market.base_hlp_vault.quote_interest_growth_index_q64, base_vault_growth);
    assert_eq!(market.quote_hlp_vault.quote_interest_growth_index_q64, quote_vault_growth);
    assert_eq!(market.quote_side.fees.ledger.credited, 1_000);
    assert_eq!(market.curve_reserve(MarketAsset::Quote).unwrap(), curve_before);
    market.assert_interest_ledger(MarketAsset::Quote).unwrap();
}

#[test]
fn accrual_nets_the_borrowing_vault_credit_against_its_funding() {
    let mut market = hlp_interest_market(10_000, 4_000, 2_000);
    let curve_before = market.curve_reserve(MarketAsset::Quote).unwrap();
    owe_quote_funding(&mut market, 1_000, 400);
    market.debt.fixed_quote_shares = 2_000;
    market.debt.fixed_quote_principal = 1_000;
    market.quote_side.reserves.live_reserve += 1_000;

    let credit = market.recognize_accrued_interest(MarketAsset::Quote).unwrap();

    // The base vault's 400 public credit burns its 400 funding interest.
    assert_eq!(credit.netted, 400);
    assert_eq!(market.base_hlp_vault.debt_shares, 1_000);
    assert_eq!(market.base_hlp_vault.debt_principal, 1_000);
    assert_eq!(market.base_hlp_vault.quote_interest_growth_index_q64, 0);
    let (quote_vault_growth, _) = distribute_growth_q64(200, 2_000, 0).unwrap();
    assert_eq!(market.quote_hlp_vault.quote_interest_growth_index_q64, quote_vault_growth);
    // Ordinary srLP keeps both its funding and public credit.
    let (funding_growth, funding_carry) = distribute_growth_q64(400, 4_000, 0).unwrap();
    let (public_growth, _) = distribute_growth_q64(400, 4_000, funding_carry).unwrap();
    assert_eq!(
        market.quote_side.fees.interest_growth_index_q64,
        funding_growth + public_growth
    );
    let ledger = market.quote_side.fees.ledger;
    assert_eq!(ledger.funding_recognized, 0);
    assert_eq!(ledger.public_recognized, 1_000);
    assert_eq!(ledger.credited, 1_000);
    assert_eq!(market.curve_reserve(MarketAsset::Quote).unwrap(), curve_before);
    market.assert_interest_ledger(MarketAsset::Quote).unwrap();
}

#[test]
fn min_liquidity_sinks_zero_holder_funding_and_later_deposit_starts_clean() {
    let mut market = hlp_interest_market(301_000, 200_000, 100_000);
    owe_quote_funding(&mut market, 100_000, 87_300);

    market.recognize_interest(MarketAsset::Quote).unwrap();

    // Only the permanently burned minimum is ordinary supply: the credit is
    // indexed over it and stays in `credited` with no claimant.
    let (expected_growth, _) = distribute_growth_q64(87_300, MIN_LIQUIDITY, 0).unwrap();
    assert_eq!(market.quote_side.fees.interest_growth_index_q64, expected_growth);
    assert_eq!(market.quote_side.fees.ledger.credited, 87_300);

    // A depositor arriving after the credit starts at this checkpoint.
    let checkpoint = market.quote_side.fees.interest_growth_index_q64;
    market.base_side.shares.srlp_supply += 50_000;
    market.quote_side.shares.srlp_supply += 50_000;
    market.recognize_interest(MarketAsset::Quote).unwrap();
    assert_eq!(market.quote_side.fees.interest_growth_index_q64, checkpoint);
    assert_eq!(
        accrue_fee_liability_with_remainder(50_000, checkpoint, checkpoint, 0).unwrap(),
        (0, 0)
    );
    market.assert_interest_ledger(MarketAsset::Quote).unwrap();
}

#[test]
fn last_ordinary_exit_clears_the_interest_carry_before_the_next_cohort() {
    let mut market = hlp_interest_market(4_000, 1_000, 1_000);
    owe_quote_funding(&mut market, 1_000, 401);
    market.recognize_interest(MarketAsset::Quote).unwrap();
    assert!(market.quote_side.fees.interest_growth_remainder_scaled > 0);
    market.base_side.fees.interest_growth_remainder_scaled = 17;

    // The 1_000 ordinary shares exit, leaving only MIN_LIQUIDITY outside the
    // two hLP vaults. Both carries end with that departing cohort.
    market.remove_liquidity(1_000).unwrap();
    assert_eq!(market.base_side.shares.srlp_supply, 3_000);
    assert_eq!(
        market.base_side.shares.srlp_supply
            - market.base_hlp_vault.srlp_shares
            - market.quote_hlp_vault.srlp_shares,
        MIN_LIQUIDITY
    );
    assert_eq!(market.base_side.fees.interest_growth_remainder_scaled, 0);
    assert_eq!(market.quote_side.fees.interest_growth_remainder_scaled, 0);

    let deposit = market.add_liquidity(1_000, 1_000).unwrap();
    let next_holder_checkpoint = market.quote_side.fees.interest_growth_index_q64;
    let (expected_next_growth, expected_next_carry) = distribute_growth_q64(137, 2_000, 0).unwrap();
    market.base_hlp_vault.debt_shares += 137;
    market.recognize_interest(MarketAsset::Quote).unwrap();

    assert_eq!(
        market.quote_side.fees.interest_growth_index_q64 - next_holder_checkpoint,
        expected_next_growth
    );
    assert_eq!(market.quote_side.fees.interest_growth_remainder_scaled, expected_next_carry);
    assert_eq!(
        accrue_fee_liability_with_remainder(
            deposit.srlp_amount,
            market.quote_side.fees.interest_growth_index_q64,
            next_holder_checkpoint,
            0,
        )
        .unwrap(),
        accrue_fee_liability_with_remainder(deposit.srlp_amount, expected_next_growth, 0, 0).unwrap()
    );
}

#[test]
fn partial_direct_burn_checkpoints_old_supply_before_reconciling() {
    let mut market = Market::current_layout();
    market.base_side.shares.srlp_supply = 10;
    market.quote_side.shares.srlp_supply = 10;
    market.base_side.fees.unallocated_swap_fee_liability = 10;
    market.base_hlp_vault.srlp_shares = 10;
    market.base_hlp_vault.hlp_supply = 10;

    reconcile_live_hlp_supply(&mut market, MarketAsset::Base, 5, false).unwrap();

    assert_eq!(market.base_hlp_vault.hlp_supply, 5);
    assert_eq!(
        market.base_hlp_vault.base_swap_fee_growth_index_q64,
        YIELD_GROWTH_SCALE_Q64
    );
    assert_eq!(market.base_hlp_vault.unallocated_base_swap_fee_amount, 0);
}

#[test]
fn direct_burn_reconciliation_rejects_external_minting_and_all_burn_zombie() {
    let mut market = Market::current_layout();
    market.quote_hlp_vault.hlp_supply = 1;
    // The vault still holds value, so only the orphan close may unwind it.
    market.quote_hlp_vault.srlp_shares = 1;

    for invalid_live_supply in [0, 2] {
        let error =
            reconcile_live_hlp_supply(&mut market, MarketAsset::Quote, invalid_live_supply, false).unwrap_err();
        match error {
            anchor_lang::error::Error::AnchorError(error) => {
                assert_eq!(error.error_name, "InvalidHlpMintSupply")
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(market.quote_hlp_vault.hlp_supply, 1);
    }
}

/// NM-205: rotation hands the side's receipt mint to the caller, so only a
/// cohort with receipts on record may retire. An idle side, or one whose last
/// holder exited, keeps its mint; a terminal cohort retires whether its
/// receipts are still live or were burned directly.
#[test]
fn only_a_cohort_with_receipts_on_record_rotates() {
    use crate::constants::NAD;
    // Mirrors `rotate_hlp_cohort`: read the supply on record, reconcile the
    // live mint supply, then retire. Returns the retired cohort's supply.
    fn rotate(market: &mut Market, target: MarketAsset, live_supply: u64) -> Result<u64> {
        let recorded = match target {
            MarketAsset::Base => market.base_hlp_vault.hlp_supply,
            MarketAsset::Quote => market.quote_hlp_vault.hlp_supply,
        };
        reconcile_live_hlp_supply(market, target, live_supply, false)?;
        Ok(market.retire_hlp_cohort(target, recorded, Pubkey::new_unique())?.hlp_supply)
    }
    let invalid_supply = error!(ErrorCode::InvalidHlpMintSupply);
    let mut market = Market::current_layout();
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.add_liquidity(1_000_000, 1_000_000).unwrap();
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        // Never minted: terminally closed in form, but nothing to retire.
        let mut idle = market.clone();
        assert!(idle.hlp_terminally_closed(target));
        assert_eq!(rotate(&mut idle, target, 0).unwrap_err(), invalid_supply);

        // The last holder exited normally.
        let mut exited = market.clone();
        let receipt = exited.deposit_single_sided(target, 100_000, 1).unwrap();
        exited.prepare_amm_for_swap(1).unwrap();
        exited.withdraw_single_sided(target, receipt.hlp_amount).unwrap();
        assert!(exited.hlp_terminally_closed(target));
        assert_eq!(rotate(&mut exited, target, 0).unwrap_err(), invalid_supply);

        // A live cohort keeps its receipts until it is terminally closed.
        let mut live = market.clone();
        let receipt = live.deposit_single_sided(target, 100_000, 1).unwrap();
        assert_eq!(
            rotate(&mut live, target, receipt.hlp_amount).unwrap_err(),
            error!(ErrorCode::HlpSettlementUnavailable)
        );

        // Terminally closed with receipts outstanding, live or burned.
        for live_supply in [3, 0] {
            let mut terminal = market.clone();
            match target {
                MarketAsset::Base => terminal.base_hlp_vault.hlp_supply = 3,
                MarketAsset::Quote => terminal.quote_hlp_vault.hlp_supply = 3,
            }
            assert_eq!(rotate(&mut terminal, target, live_supply).unwrap(), live_supply);
            let fresh = match target {
                MarketAsset::Base => terminal.base_hlp_vault,
                MarketAsset::Quote => terminal.quote_hlp_vault,
            };
            assert_eq!(fresh.hlp_supply, 0);
            assert_ne!(fresh.srlp_vault, Pubkey::default());
        }
    }
}

#[test]
fn complete_direct_burn_is_recoverable() {
    // A vault with value keeps its stored supply for the orphan close, which
    // unwinds it for the whole stored supply.
    let mut market = Market::current_layout();
    market.quote_hlp_vault.hlp_supply = 3;
    market.quote_hlp_vault.srlp_shares = 1;
    reconcile_live_hlp_supply(&mut market, MarketAsset::Quote, 0, true).unwrap();
    assert_eq!(market.quote_hlp_vault.hlp_supply, 3);
    // A terminally closed vault holds no value; its supply follows the mint so
    // the cohort can rotate.
    let mut terminal = Market::current_layout();
    terminal.quote_hlp_vault.hlp_supply = 3;
    reconcile_live_hlp_supply(&mut terminal, MarketAsset::Quote, 0, false).unwrap();
    assert_eq!(terminal.quote_hlp_vault.hlp_supply, 0);
}
