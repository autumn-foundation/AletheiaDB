use aletheiadb::core::id::{IdGenerator};

#[test]
fn test_id_generator_overflow_wrap() {
    // Start at u64::MAX
    let generator = IdGenerator::with_start(u64::MAX);

    // First call will return u64::MAX and increment to 0
    let res1 = generator.next();
    assert!(res1.is_err()); // correctly errors because u64::MAX > MAX_VALID_ID

    // Second call will return 0 and increment to 1
    let res2 = generator.next();

    // It should STILL be an error! But the bug makes it Ok(0)!
    assert!(
        res2.is_err(),
        "VULNERABILITY: Generator wrapped around and produced valid ID after overflow!"
    );
}
