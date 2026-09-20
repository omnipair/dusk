use super::*;

#[test]
fn service_fee_is_ten_bps_without_dust_or_overflow_bypass() {
    assert_eq!(order_protocol_fee(0), 0);
    assert_eq!(order_protocol_fee(1), 1);
    assert_eq!(order_protocol_fee(999), 1);
    assert_eq!(order_protocol_fee(1_000), 1);
    assert_eq!(order_protocol_fee(1_001), 2);
    assert_eq!(order_protocol_fee(10_000_000), 10_000);
    assert_eq!(order_protocol_fee(u64::MAX), u64::MAX / 1_000 + 1);
    for total in [1, 999, 1_000, 1_001, 50_019, u64::MAX] {
        let a = total / 3;
        assert!(order_protocol_fee(a) + order_protocol_fee(total - a) >= order_protocol_fee(total));
    }
}
