use super::*;

    #[test]
    fn virtual_reserves_match_spot_when_pessimistic_price_matches_spot() {
        let x = 1_000 * NAD as u128;
        let y = 500 * NAD as u128;

        let (x_virt, y_virt) =
            construct_normalized_virtual_reserves_at_pessimistic_price(x, y, NAD / 2, NAD / 2)
                .unwrap();

        assert_eq!(x_virt, x);
        assert_eq!(y_virt, y);
    }

    #[test]
    fn virtual_reserves_move_to_lower_pessimistic_price_coordinate() {
        let x = 1_000 * NAD as u128;
        let y = 1_000 * NAD as u128;

        let (x_virt, y_virt) =
            construct_normalized_virtual_reserves_at_pessimistic_price(x, y, 2 * NAD, NAD / 4)
                .unwrap();

        assert_eq!(x_virt, 2_000 * NAD as u128);
        assert_eq!(y_virt, 500 * NAD as u128);
        assert_eq!(
            x_virt.checked_mul(y_virt).unwrap(),
            x.checked_mul(y).unwrap()
        );
    }

    #[test]
    fn raw_amount_out_matches_constant_product_rounding_down() {
        let dy = calculate_raw_amount_out(1_000, 2_000, 100).unwrap();

        assert_eq!(dy, 181);
    }

    #[test]
    fn raw_amount_in_is_the_minimum_exact_output_input() {
        let target_out = 181;
        let amount_in = calculate_raw_amount_in(1_000, 2_000, target_out).unwrap();

        assert_eq!(amount_in, 100);
        assert!(calculate_raw_amount_out(1_000, 2_000, amount_in).unwrap() >= target_out);
        assert!(calculate_raw_amount_out(1_000, 2_000, amount_in - 1).unwrap() < target_out);
    }

    #[test]
    fn raw_amount_in_rejects_draining_the_output_reserve() {
        assert!(calculate_raw_amount_in(1_000, 2_000, 2_000).is_err());
    }
