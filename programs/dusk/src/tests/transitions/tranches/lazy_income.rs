use super::*;

#[test]
fn lazy_income_preserves_layout_and_mutations() {
    let mut expected = MarketIncomeBooks::default();
    assert_eq!(
        LazyIncomeBooks::default().try_to_vec().unwrap(),
        expected.try_to_vec().unwrap()
    );
    expected.pending = [1, 2, 3, 4];
    expected.split_carry_bps[1] = 7_500;
    let bytes = expected.try_to_vec().unwrap();
    assert_eq!(bytes.len(), LazyIncomeBooks::INIT_SPACE);
    let mut lazy = LazyIncomeBooks::try_from_slice(&bytes).unwrap();
    assert!(lazy.decoded.get().is_some());
    assert_eq!(lazy.try_to_vec().unwrap(), bytes);
    assert_eq!(lazy.get().unwrap().pending, expected.pending);
    assert_eq!(lazy.try_to_vec().unwrap(), bytes);
    let sibling = lazy.clone();
    lazy.get_mut().unwrap().pending[0] = 99;
    assert_eq!(sibling.get().unwrap().pending[0], 1);
    assert_eq!(sibling.try_to_vec().unwrap(), bytes);
    expected.pending[0] = 99;
    assert_eq!(lazy.try_to_vec().unwrap(), expected.try_to_vec().unwrap());
    let before_reload = lazy.clone();
    lazy.reload(&mut bytes.as_slice()).unwrap();
    assert_eq!(lazy.get().unwrap().pending[0], 1);
    assert_eq!(before_reload.get().unwrap().pending[0], 99);
    assert_eq!(before_reload.try_to_vec().unwrap(), expected.try_to_vec().unwrap());
    let mut encoded_only = LazyIncomeBooks::try_from_slice(&bytes).unwrap();
    encoded_only
        .reload(&mut expected.try_to_vec().unwrap().as_slice())
        .unwrap();
    assert_eq!(encoded_only.get().unwrap().pending[0], 99);
}
