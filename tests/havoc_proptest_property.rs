use aletheiadb::core::property::PropertyValue;
use proptest::prelude::*;
use std::sync::Arc;

proptest! {
    // Fuzz the deserializer with arbitrary byte arrays
    #[test]
    fn fuzz_deserialize_arbitrary_bytes(bytes in any::<Vec<u8>>()) {
        // We just want to ensure this doesn't panic.
        // It's expected to return errors for most random data.
        let _ = PropertyValue::deserialize(&bytes);
    }

    // Fuzz with deeply nested recursive structures
    // This constructs valid serialized data that is deeply nested
    #[test]
    fn fuzz_deep_recursion(depth in 0..200usize) {
        // Manually construct a nested array structure
        // [TAG_ARRAY][count=1] ... [TAG_NULL]
        let mut bytes = Vec::new();

        // Tags
        const TAG_NULL: u8 = 0;
        const TAG_ARRAY: u8 = 6;

        for _ in 0..depth {
            bytes.push(TAG_ARRAY);
            // Count = 1
            bytes.extend_from_slice(&1u32.to_le_bytes());
        }
        bytes.push(TAG_NULL);

        // This should return Ok for small depth, Err for large depth
        // But crucially, it MUST NOT panic (stack overflow).
        let _ = PropertyValue::deserialize(&bytes);
    }
}
