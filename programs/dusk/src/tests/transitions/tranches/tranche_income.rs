use super::*;

#[test]
fn tranche_income_encoding_matches_borsh_field_order() {
    let mut bytes = vec![0_u8; TrancheIncome::INIT_SPACE];
    bytes[0] = 1;
    for (index, byte) in bytes[1..131].iter_mut().enumerate() {
        *byte = index as u8;
    }
    bytes[131..133].copy_from_slice(&9_999_u16.to_le_bytes());
    bytes[133..135].copy_from_slice(&5_000_u16.to_le_bytes());
    let parsed = TrancheIncome::try_from_slice(&bytes).unwrap();
    assert!(parsed.initialized);
    assert_eq!(parsed.carry.interest_remainder_bps, 9_999);
    assert_eq!(parsed.carry.swap_remainder_bps, 5_000);
    assert_eq!(parsed.reserve_shares, u64::from_le_bytes([2, 3, 4, 5, 6, 7, 8, 9]));
    assert_eq!(
        parsed.seed_interest_remainder,
        u64::from_le_bytes([122, 123, 124, 125, 126, 127, 128, 129])
    );
    assert_eq!(parsed.try_to_vec().unwrap(), bytes);
    for i in [0] {
        let mut invalid = bytes.clone();
        invalid[i] = 2;
        assert!(TrancheIncome::try_from_slice(&invalid).is_err());
    }
}
