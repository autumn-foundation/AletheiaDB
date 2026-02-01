use gallifreydb::core::property::{PropertyValue, deserialize_vector, serialize_vector};

#[test]
fn test_vector_deserialization_safety_truncated() {
    // Valid vector
    let vec_data = vec![1.0f32, 2.0, 3.0];
    let bytes = serialize_vector(&vec_data);

    // Truncate the buffer (remove last byte)
    let truncated = &bytes[0..bytes.len() - 1];

    // This should fail gracefully, NOT panic or UB
    let result = deserialize_vector(truncated);
    assert!(result.is_err());
}

#[test]
fn test_vector_deserialization_safety_malformed_header() {
    // Malformed dimension that would cause overflow if not checked
    // [TAG_VECTOR, u32::MAX, ...]
    let mut bytes = vec![7u8]; // TAG_VECTOR
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    // Add some dummy data
    bytes.extend_from_slice(&[0u8; 100]);

    // Should fail due to max dimension check or size mismatch
    let result = deserialize_vector(&bytes);
    assert!(result.is_err());
}

#[test]
fn test_property_map_deserialization_safety_recursion() {
    // Construct a deeply nested property to test recursion limits
    // Array of Array of ...

    let mut bytes = Vec::new();
    let depth = 200; // Above limit 100

    for _ in 0..depth {
        bytes.push(6); // TAG_ARRAY
        bytes.extend_from_slice(&1u32.to_le_bytes()); // Count 1
    }
    bytes.push(0); // TAG_NULL at the bottom

    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(format!("{}", err).contains("recursion"));
}
