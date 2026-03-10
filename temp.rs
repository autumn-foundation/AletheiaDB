#[test]
fn test_sentry_max_valid_timestamp_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    // Test a timestamp that is valid under `i64::MAX - 1000` but invalid under `i64::MAX / 1000`.
    let large_val = i64::MAX - 2000;

    // We cannot use `new_unchecked` here because it is `pub(crate)`.
    // Let's use `HybridTimestamp::new` instead. It does internal validation,
    // but the `TimeRange` constructor is what the mutants apply to.
    let ts = HybridTimestamp::new(large_val, 0).unwrap();

    let result = TimeRange::new(ts, ts);
    assert!(
        result.is_ok(),
        "Should accept large timestamp close to i64::MAX"
    );
}

#[test]
fn test_serialization_endianness() {
    use aletheiadb::core::temporal::TimeRange;
    use aletheiadb::core::hlc::HybridTimestamp;

    // Create valid timestamps using the public API.
    let ts1 = HybridTimestamp::new(0x0102030405060708, 0).unwrap();
    let ts2 = HybridTimestamp::new(0x1112131415161718, 0).unwrap();
    let range = TimeRange::new(ts1, ts2).unwrap();
    let bytes = range.serialize();

    // Check start wallclock (little endian)
    assert_eq!(&bytes[0..8], &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
}
