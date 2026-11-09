use super::*;

#[test]
fn in_place_refresh_matches_canonical_decoder_and_reuses_boxes() {
    let mut bytes = Vec::new();
    Market::default().try_serialize(&mut bytes).unwrap();
    let mut target = Market::default();
    let pointers = (
        &*target.base_side as *const MarketSide,
        &*target.quote_side as *const MarketSide,
        &*target.amm as *const AmmState,
    );
    // Perturb every wire byte. Valid canonical encodings must refresh the
    // entire account, including fields not currently used by protection.
    for index in 8..bytes.len() {
        let original = bytes[index];
        bytes[index] = original.wrapping_add(1);
        if let Ok(expected) = Market::try_deserialize(&mut bytes.as_slice()) {
            target.reload_from_account_bytes(&bytes).unwrap();
            let mut actual_bytes = Vec::new();
            let mut expected_bytes = Vec::new();
            target.try_serialize(&mut actual_bytes).unwrap();
            expected.try_serialize(&mut expected_bytes).unwrap();
            assert_eq!(actual_bytes, expected_bytes, "wire byte {index}");
        }
        bytes[index] = original;
    }
    assert_eq!(
        pointers,
        (
            &*target.base_side as *const MarketSide,
            &*target.quote_side as *const MarketSide,
            &*target.amm as *const AmmState,
        )
    );
    assert!(target.reload_from_account_bytes(&bytes[..bytes.len() - 1]).is_err());
    bytes[0] ^= 1;
    assert!(target.reload_from_account_bytes(&bytes).is_err());
}
