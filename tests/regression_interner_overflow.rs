use aletheiadb::core::interning::StringInterner;

#[test]
#[cfg(target_pointer_width = "64")]
fn test_interner_large_capacity_overflow() {
    // Set capacity to 2^32. On 64-bit, this fits in usize.
    // If we cast to u32 without clamping, it becomes 0, causing immediate CapacityExceeded.
    let large_capacity: usize = 1usize << 32;
    let interner = StringInterner::with_max_capacity(large_capacity);

    // With the fix, effective capacity should be u32::MAX, so this should succeed.
    let result = interner.intern("test_string");

    assert!(
        result.is_ok(),
        "Expected intern to succeed with large capacity, but got error: {:?}",
        result.err()
    );
}
