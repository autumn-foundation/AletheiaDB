use aletheiadb::core::id::TxIdGenerator;

#[test]
#[should_panic(expected = "Transaction ID overflow")]
fn test_tx_id_overflow_panic() {
    // Start at MAX-1.
    // next() -> MAX-1
    // next() -> MAX
    // next() -> Should panic
    let generator = TxIdGenerator::with_start(u64::MAX - 1);

    let id1 = generator.next();
    assert_eq!(id1.as_u64(), u64::MAX - 1);

    let id2 = generator.next();
    assert_eq!(id2.as_u64(), u64::MAX);

    // This should panic
    let _id3 = generator.next();
}
