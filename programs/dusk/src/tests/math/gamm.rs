use super::*;

#[test]
fn virtual_reserves_match_spot_when_pessimistic_price_matches_spot() {
    let x = 1_000 * NAD as u128;
    let y = 500 * NAD as u128;

    let (x_virt, y_virt) = construct_normalized_virtual_reserves_at_pessimistic_price(x, y, NAD / 2, NAD / 2).unwrap();

    assert_eq!(x_virt, x);
    assert_eq!(y_virt, y);
}

#[test]
fn virtual_reserves_move_to_lower_pessimistic_price_coordinate() {
    let x = 1_000 * NAD as u128;
    let y = 1_000 * NAD as u128;

    let (x_virt, y_virt) = construct_normalized_virtual_reserves_at_pessimistic_price(x, y, 2 * NAD, NAD / 4).unwrap();

    assert_eq!(x_virt, 2_000 * NAD as u128);
    assert_eq!(y_virt, 500 * NAD as u128);
    assert_eq!(x_virt.checked_mul(y_virt).unwrap(), x.checked_mul(y).unwrap());
}

#[test]
fn raw_amount_out_matches_constant_product_rounding_down() {
    let dy = calculate_raw_amount_out(1_000, 2_000, 100).unwrap();

    assert_eq!(dy, 181);
}

#[test]
fn normalized_cpmm_uses_full_width_products_without_changing_raw_rounding() {
    let scale = NAD as u128;
    let x_raw = 9_223_372_036_854_775_807_u128;
    let y_raw = 6_148_914_691_236_517_205_u128;
    let dx_raw = 4_611_686_018_427_387_903_u128;
    let dy_raw = 1_844_674_407_370_955_161_u128;
    let x = x_raw * scale;
    let y = y_raw * scale;
    let dx = dx_raw * scale;
    let dy = dy_raw * scale;

    assert!(dx.checked_mul(y).is_none());
    let amount_out_nad = calculate_normalized_amount_out(x, y, dx).unwrap();
    assert_eq!(amount_out_nad / scale, 2_049_638_230_412_172_401);

    assert!(dy.checked_mul(x).is_none());
    let amount_in_nad = calculate_normalized_amount_in(x, y, dy).unwrap();
    assert_eq!(amount_in_nad.div_ceil(scale), 3_952_873_730_080_618_202);

    assert_eq!(calculate_normalized_amount_in(u128::MAX, u128::MAX, 1).unwrap(), 2);
    assert!(calculate_normalized_amount_in(u128::MAX, 3, 2).is_err());
}

#[test]
fn conservative_k_reconstruction_preserves_spot_ratio() {
    let x = 4_000 * NAD as u128;
    let y = 1_000 * NAD as u128;
    let conservative_k = (1_000 * NAD as u128).pow(2);

    let (x_depth, y_depth) = construct_normalized_reserves_from_k_at_spot_ratio(x, y, conservative_k).unwrap();

    assert_eq!(x_depth, 2_000 * NAD as u128);
    assert_eq!(y_depth, 500 * NAD as u128);
    assert_eq!(x_depth * y_depth, conservative_k);
    assert_eq!(x_depth * y, y_depth * x);
}

#[test]
fn conservative_k_reconstruction_never_expands_spot_depth() {
    let x = 700 * NAD as u128;
    let y = 1_300 * NAD as u128;
    let spot_k = x * y;

    let (x_depth, y_depth) = construct_normalized_reserves_from_k_at_spot_ratio(x, y, spot_k * 2).unwrap();

    assert_eq!((x_depth, y_depth), (x, y));
}
