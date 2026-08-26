use super::*;
use crate::constants::{MS_PER_DAY, TARGET_MS_PER_SLOT};

#[test]
fn daily_limit_bucket_decays_over_one_day() {
    let mut limits = DailyBorrowBucket {
        borrowed_bucket: 100_000,
        last_decay_slot: 0,
        decay_remainder_ms: 0,
    };
    let half_day_slots = MS_PER_DAY / TARGET_MS_PER_SLOT / 2;

    limits.decay_to_slot(100_000, half_day_slots).unwrap();

    assert_eq!(limits.borrowed_bucket, 50_000);
}

#[test]
fn refill_is_independent_of_checkpoint_frequency_for_a_fixed_limit() {
    let limit = 100_003;
    let elapsed_slots = 10_001;
    let first_segment = 3_333;
    let second_segment = 3_333;
    let mut single = DailyBorrowBucket {
        borrowed_bucket: limit,
        ..DailyBorrowBucket::default()
    };
    let mut split = single;

    single.decay_to_slot(limit, elapsed_slots).unwrap();
    split.decay_to_slot(limit, first_segment).unwrap();
    split
        .decay_to_slot(limit, first_segment + second_segment)
        .unwrap();
    split.decay_to_slot(limit, elapsed_slots).unwrap();

    assert_eq!(split.borrowed_bucket, single.borrowed_bucket);
    assert_eq!(split.decay_remainder_ms, single.decay_remainder_ms);
}

#[test]
fn a_full_bucket_refills_completely_after_one_day() {
    let limit = 100_000;
    let day_slots = MS_PER_DAY / TARGET_MS_PER_SLOT;
    let mut limits = DailyBorrowBucket {
        borrowed_bucket: limit,
        ..DailyBorrowBucket::default()
    };

    limits.decay_to_slot(limit, day_slots).unwrap();

    assert_eq!(limits.borrowed_bucket, 0);
    assert_eq!(limits.decay_remainder_ms, 0);
    assert_eq!(limits.remaining(limit, day_slots).unwrap(), limit);
}
