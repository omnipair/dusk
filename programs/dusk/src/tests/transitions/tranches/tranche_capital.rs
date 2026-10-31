use super::*;
use proptest::prelude::*;

#[test]
fn creator_seed_is_funded_once_and_not_a_second_claim() {
    let mut capital = TrancheCapital::default();
    assert_eq!(capital.seed_from_creator(100_000, 101_000).unwrap(), 100);
    assert_eq!(capital.permanent_receipts, 100);
    assert_eq!(capital.senior_reserve_shares(101_000).unwrap(), 100_900);
    assert!(capital.seed_from_creator(100_000, 101_000).is_err());
    assert_eq!(capital.junior_value(101_000, 101_000).unwrap(), 100);
}

#[test]
fn junior_absorbs_both_assets_without_reusing_the_same_budget() {
    let mut capital = TrancheCapital::default();
    let mut total = 100;
    capital.enter(20, total).unwrap();
    let base_loss = capital.absorb_loss(100, 12, &mut total).unwrap();
    assert_eq!(base_loss.senior_loss_value, 0);
    assert_eq!((total, capital.junior_reserve_shares), (88, 8));
    assert_eq!(capital.senior_reserve_shares(total).unwrap(), 80);
    let quote_loss = capital.absorb_loss(88, 15, &mut total).unwrap();
    assert_eq!(quote_loss.junior_value_before, 8);
    assert_eq!(quote_loss.senior_loss_value, 7);
    assert!(quote_loss.junior_wiped);
    assert_eq!((total, capital.junior_reserve_shares), (80, 0));
}

#[test]
fn exit_uses_current_value_and_does_not_withdraw_cash() {
    let mut capital = TrancheCapital::default();
    let mut total = 100;
    capital.enter(20, total).unwrap();
    capital.queued_receipts = 10;
    capital.absorb_loss(100, 10, &mut total).unwrap();
    assert_eq!(capital.convert_queued(10).unwrap(), 5);
    assert_eq!(total, 90);
    assert_eq!(capital.senior_reserve_shares(total).unwrap(), 85);
    assert_eq!(capital.junior_reserve_shares, 5);
    assert_eq!(capital.junior_receipt_supply, 10);
}

#[test]
fn wipe_requires_new_generation_and_does_not_revive_old_receipts() {
    let mut capital = TrancheCapital::default();
    let mut total = 100;
    capital.enter(20, total).unwrap();
    capital.absorb_loss(100, 20, &mut total).unwrap();
    assert!(capital.enter(5, total).is_err());
    let retired = capital.retire_wiped_generation().unwrap();
    assert_eq!(retired.junior_receipt_supply, 20);
    assert_eq!(retired.junior_reserve_shares, 0);
    assert_eq!(capital.generation, 1);
    assert_eq!(capital.enter(5, total).unwrap(), 5);
    assert_eq!(capital.junior_receipt_supply, 5);
}

#[test]
fn request_fee_changes_ownership_not_backing_or_total_receipts() {
    let mut capital = TrancheCapital::default();
    capital.enter(20_000, 100_000).unwrap();
    capital.pay_seed_fee(10).unwrap();
    assert_eq!(capital.permanent_receipts, 10);
    assert_eq!(capital.junior_receipt_supply, 20_000);
    assert_eq!(capital.junior_reserve_shares, 20_000);
    capital.queued_receipts = 19_990;
    assert!(capital.pay_seed_fee(1).is_err());
}

proptest! {
    #[test]
    fn senior_units_are_preserved_and_backing_conserved(
        total in 1_000_u64..=1_000_000_000,
        junior_bps in 0_u64..=10_000,
        loss_bps in 0_u64..=10_000,
        unit_value in 1_u64..=1_000_000,
    ) {
        let mut capital = TrancheCapital::default();
        let junior = total * junior_bps / 10_000;
        if junior > 0 { capital.enter(junior, total).unwrap(); }
        let senior = total - junior;
        let value_before = total as u128 * unit_value as u128;
        let loss = value_before * loss_bps as u128 / 10_000;
        let mut after_total = total;
        let receipt = capital.absorb_loss(value_before, loss, &mut after_total).unwrap();
        assert_eq!(capital.senior_reserve_shares(after_total).unwrap(), senior);
        capital.validate(after_total).unwrap();
        assert!(receipt.reserve_shares_burned <= junior);
        if receipt.senior_loss_value == 0 && after_total > 0 {
            let after_value = (value_before - loss) * senior as u128 / after_total as u128;
            assert!(after_value >= senior as u128 * unit_value as u128);
        }
    }
}

#[test]
fn sub_thousand_atom_creator_deposit_can_still_fund_its_seed() {
    let mut capital = TrancheCapital::default();
    assert_eq!(capital.seed_from_creator(17, 1_017).unwrap(), 1);
    assert_eq!(capital.permanent_receipts, 1);
    assert_eq!(capital.junior_reserve_shares, 1);
    assert_eq!(capital.senior_reserve_shares(1_017).unwrap(), 1_016);
}

#[test]
fn tranche_capital_encoding_matches_fixed_borsh_field_order() {
    let capital = TrancheCapital {
        junior_reserve_shares: 0x1020_3040_5060_7080,
        junior_receipt_supply: u64::MAX,
        permanent_receipts: 17,
        queued_receipts: 23,
        generation: 42,
    };
    let expected: Vec<u8> = [capital.junior_reserve_shares, capital.junior_receipt_supply,
        capital.permanent_receipts, capital.queued_receipts, capital.generation]
        .into_iter().flat_map(u64::to_le_bytes).collect();
    assert_eq!(capital.try_to_vec().unwrap(), expected);
    assert_eq!(TrancheCapital::try_from_slice(&expected).unwrap(), capital);
    assert!(TrancheCapital::try_from_slice(&expected[..39]).is_err());
}
