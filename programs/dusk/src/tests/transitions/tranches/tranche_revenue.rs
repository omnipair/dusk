use super::*;
use proptest::prelude::*;

#[test]
fn all_lp_fee_sources_use_the_agreed_split() {
    let mut capital = TrancheCapital::default();
    capital.enter(20, 100).unwrap();
    let mut carry = TrancheRevenueCarry::default();
    for source in [TrancheRevenueSource::BorrowInterest, TrancheRevenueSource::SwapFee] {
        assert_eq!(
            carry.allocate(100, source, &capital).unwrap(),
            TrancheRevenueAllocation {
                senior: 50,
                junior: 50,
                rebuild_seed: 0
            }
        );
    }
    assert_eq!(
        carry
            .allocate(100, TrancheRevenueSource::LiquidationCoverage, &capital)
            .unwrap(),
        TrancheRevenueAllocation {
            senior: 0,
            junior: 100,
            rebuild_seed: 0
        }
    );
}

#[test]
fn empty_and_wiped_junior_rebuild_seed_without_gifting_the_next_depositor() {
    let mut capital = TrancheCapital::default();
    let mut carry = TrancheRevenueCarry::default();
    let empty = carry
        .allocate(100, TrancheRevenueSource::BorrowInterest, &capital)
        .unwrap();
    assert_eq!(
        empty,
        TrancheRevenueAllocation {
            senior: 50,
            junior: 0,
            rebuild_seed: 50
        }
    );
    capital.enter(20, 100).unwrap();
    capital.absorb_loss(100, 20, &mut 100).unwrap();
    assert_eq!(
        carry.allocate(100, TrancheRevenueSource::SwapFee, &capital).unwrap(),
        empty
    );
    assert_eq!(
        carry
            .allocate(100, TrancheRevenueSource::LiquidationCoverage, &capital)
            .unwrap(),
        TrancheRevenueAllocation {
            senior: 0,
            junior: 0,
            rebuild_seed: 100
        }
    );
}

#[test]
fn one_atom_payments_cannot_starve_junior_and_lanes_do_not_share_carry() {
    let mut capital = TrancheCapital::default();
    capital.enter(20, 100).unwrap();
    let mut carry = TrancheRevenueCarry::default();
    let a = carry.allocate(1, TrancheRevenueSource::SwapFee, &capital).unwrap();
    assert_eq!(a.senior, 1);
    assert_eq!(
        carry
            .allocate(1, TrancheRevenueSource::BorrowInterest, &capital)
            .unwrap()
            .senior,
        1
    );
    assert_eq!(
        carry
            .allocate(1, TrancheRevenueSource::SwapFee, &capital)
            .unwrap()
            .junior,
        1
    );
    assert_eq!(
        carry
            .allocate(1, TrancheRevenueSource::BorrowInterest, &capital)
            .unwrap()
            .junior,
        1
    );
}

proptest! {
    #[test]
    fn allocations_conserve_collected_atoms(amount in any::<u64>(), initial_carry in any::<bool>(), wiped in any::<bool>()) {
        let mut capital = TrancheCapital::default();
        if !wiped { capital.enter(20, 100).unwrap(); }
        let mut carry = TrancheRevenueCarry { swap_half_atom: initial_carry, ..Default::default() };
        let result = carry.allocate(amount, TrancheRevenueSource::SwapFee, &capital).unwrap();
        assert_eq!(result.senior as u128 + result.junior as u128 + result.rebuild_seed as u128, amount as u128);
        assert_eq!(2 * (result.junior as u128 + result.rebuild_seed as u128) + u128::from(carry.swap_half_atom), amount as u128 + u128::from(initial_carry));
    }
}
