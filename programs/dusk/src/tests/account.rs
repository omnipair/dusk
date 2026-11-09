use super::*;

/// `YieldAccount` before and after it gained `harvest_authority`, which
/// is an `Option<Pubkey>`: one discriminant byte and thirty-two of key.
/// Every yield account on the devnet market was created at the smaller
/// size, and adding liquidity through one failed with `InvalidArgument`
/// until this grew instead of rejecting.
const YIELD_ACCOUNT_BEFORE: usize = 234;
const YIELD_ACCOUNT_AFTER: usize = 267;

#[test]
fn a_pda_at_the_current_size_is_left_alone() {
    assert!(!existing_pda_needs_growth(YIELD_ACCOUNT_AFTER, YIELD_ACCOUNT_AFTER).unwrap());
}

#[test]
fn a_pda_from_before_a_new_field_grows() {
    assert_eq!(YIELD_ACCOUNT_AFTER - YIELD_ACCOUNT_BEFORE, 1 + 32);
    assert!(existing_pda_needs_growth(YIELD_ACCOUNT_BEFORE, YIELD_ACCOUNT_AFTER).unwrap());
}

#[test]
fn a_pda_larger_than_its_state_is_still_refused() {
    // Shrinking would truncate whatever those bytes hold, so this stays
    // an error rather than becoming a silent narrowing.
    assert!(existing_pda_needs_growth(YIELD_ACCOUNT_AFTER, YIELD_ACCOUNT_BEFORE).is_err());
}
