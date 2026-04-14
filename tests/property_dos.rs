use aletheiadb::core::property::{PropertyValue, TAG_ARRAY, TAG_NULL};

#[test]
fn test_recursive_array_stack_overflow_protection() {
    // Note: Only arrays support nesting. Vectors contain primitives (f32/f64)
    // and cannot trigger recursion-based stack overflow.

    // Construct a deeply nested array: [[[[...]]]]
    // Format: [TAG_ARRAY][count:1][TAG_ARRAY][count:1]...[TAG_NULL]
    let depth = 150;
    let mut bytes = Vec::new();

    for _ in 0..depth {
        bytes.push(TAG_ARRAY);
        bytes.extend_from_slice(&(1u32).to_le_bytes()); // Count = 1
    }

    // Terminate with a Null value
    bytes.push(TAG_NULL);

    // Try to deserialize
    let result = PropertyValue::deserialize(&bytes);

    // Should fail gracefully with a recursion limit error
    match result {
        Ok(_) => panic!("Deserialization should have failed due to recursion depth limit"),
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains("recursion depth limit exceeded") {
                panic!("Expected recursion limit error, got: {}", msg);
            }
        }
    }
}
