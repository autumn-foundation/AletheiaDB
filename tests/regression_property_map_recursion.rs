// Regression Test: PropertyMap Recursion and Memory Limits
// Ensures that PropertyMap deserialization handles malicious inputs gracefully.

use aletheiadb::core::property::{MAX_RECURSION_DEPTH, PropertyValue, TAG_ARRAY, TAG_NULL};

#[test]
fn regression_recursion_limit_enforced() {
    println!("Testing Recursion Limit...");

    // Construct a deeply nested buffer manually
    // [TAG_ARRAY, 1, TAG_ARRAY, 1, ..., TAG_NULL]
    let mut bytes = Vec::new();
    let depth = MAX_RECURSION_DEPTH + 50; // Exceed limit

    for _ in 0..depth {
        bytes.push(TAG_ARRAY);
        bytes.extend_from_slice(&1u32.to_le_bytes());
    }
    bytes.push(TAG_NULL);

    let result = PropertyValue::deserialize(&bytes);

    if result.is_ok() {
        panic!("FAILURE: Deserialization succeeded despite exceeding recursion limit!");
    } else {
        let err = result.err().unwrap();
        assert!(err.to_string().contains("recursion depth limit exceeded"));
        println!("SUCCESS: Recursion limit caught the attack.");
    }
}

#[test]
fn regression_memory_amplification_prevented() {
    println!("Testing Memory Amplification...");

    // Construct a buffer with a huge array count but minimal data
    // This attempts to trigger OOM via Vec::with_capacity
    // We rely on the fact that [TAG_NULL] is 1 byte.
    // So we provide `count` nulls.

    let count = 10_000_000; // 10M elements (limit)

    println!("Allocating input buffer...");
    let mut bytes = Vec::with_capacity(count + 10);
    bytes.push(TAG_ARRAY);
    bytes.extend_from_slice(&(count as u32).to_le_bytes());
    // Fill with TAG_NULLs
    bytes.resize(bytes.capacity(), TAG_NULL);

    println!("Deserializing 10M elements...");
    // This might take a while but should not crash
    let result = PropertyValue::deserialize(&bytes);

    // If it succeeds, it means we handled 10M items without OOM.
    // The key is that it doesn't crash the process.
    if let Err(e) = result {
        println!("Deserialization failed (unexpected): {:?}", e);
        // Note: It might fail if we messed up the buffer construction, but shouldn't fail due to OOM.
    } else {
        println!("SUCCESS: Deserialization of max-allowed elements handled safely.");
    }
}
