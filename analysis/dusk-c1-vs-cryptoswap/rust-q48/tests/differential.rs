use dusk_c1_q48_reference::{BridgeStart, FiniteC1};

fn parse_row(row: &str) -> Vec<u128> {
    row.split(',')
        .map(|value| value.parse::<u128>().unwrap())
        .collect()
}

fn bridge_from_numerator(value: u128) -> BridgeStart {
    match value {
        1 => BridgeStart::Quarter,
        2 => BridgeStart::Half,
        _ => panic!("invalid bridge numerator"),
    }
}

#[test]
fn analytical_quotes_match_within_one_raw_atom() {
    let fixture = include_str!("fixtures.csv");
    let mut rows = fixture.lines();
    assert_eq!(
        rows.next().unwrap(),
        "peak_depth_nad,fade_scale_nad,bridge_numerator,reserve_in,reserve_out,amount_in,expected_amount_out_floor"
    );
    let mut checked = 0_usize;
    for row in rows {
        let values = parse_row(row);
        assert_eq!(values.len(), 7);
        let bridge = bridge_from_numerator(values[2]);
        let model = FiniteC1::new(values[0], values[1], bridge).unwrap();
        let quote = model
            .quote_exact_in(
                u64::try_from(values[3]).unwrap(),
                u64::try_from(values[4]).unwrap(),
                u64::try_from(values[5]).unwrap(),
            )
            .unwrap_or_else(|error| panic!("quote failed: row={row}, error={error:?}"));
        let expected = u64::try_from(values[6]).unwrap();
        assert!(
            quote.amount_out.abs_diff(expected) <= 1,
            "quote mismatch: row={row}, actual={}",
            quote.amount_out
        );
        checked += 1;
    }
    assert!(checked >= 500);
}

#[test]
fn analytical_exact_out_quotes_match_within_one_raw_atom() {
    let fixture = include_str!("fixtures_exact_out.csv");
    let mut rows = fixture.lines();
    assert_eq!(
        rows.next().unwrap(),
        "peak_depth_nad,fade_scale_nad,bridge_numerator,reserve_in,reserve_out,amount_out,expected_amount_in_ceil"
    );
    let mut checked = 0_usize;
    for row in rows {
        let values = parse_row(row);
        assert_eq!(values.len(), 7);
        let model = FiniteC1::new(values[0], values[1], bridge_from_numerator(values[2])).unwrap();
        let quote = model
            .quote_exact_out(
                u64::try_from(values[3]).unwrap(),
                u64::try_from(values[4]).unwrap(),
                u64::try_from(values[5]).unwrap(),
            )
            .unwrap_or_else(|error| panic!("quote failed: row={row}, error={error:?}"));
        let expected = u64::try_from(values[6]).unwrap();
        assert!(
            quote.amount_in.abs_diff(expected) <= 1,
            "quote mismatch: row={row}, actual={}",
            quote.amount_in
        );
        checked += 1;
    }
    assert!(checked >= 500);
}
