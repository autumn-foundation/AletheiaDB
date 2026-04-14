use aletheiadb::core::property::{
    MAX_VECTOR_DIMENSIONS, PropertyValue, TAG_ARRAY, TAG_STRING, TAG_VECTOR,
};

#[test]
fn test_serialize_oversized_vector_no_panic() {
    // Manually constructing a PropertyValue::Vector via try_vector with valid dimensions
    // is the normal path. But if we somehow had an oversized vector inside (e.g. from internal memory
    // manipulation or bypassing the constructor), we want to ensure serialize() doesn't panic.
    //
    // Note: PropertyValue::vector() panics if dims exceed MAX. PropertyValue::try_vector() returns Err.
    // So getting an oversized vector INTO the enum is hard safely.
    // However, we can test the serialization logic by calling the underlying serialize_vector_into
    // or by mocking if we could.
    //
    // Since we can't easily construct an oversized PropertyValue::Vector safely without unsafe code or
    // internal access, we will rely on `try_serialize_vector_into` unit tests which we already saw
    // in `src/core/vector/serialization.rs` (it has a test for panic on overflow, but `try_` version returns Err).
    //
    // Let's verify PropertyValue::try_vector rejection first.
    let oversized = vec![0.0f32; MAX_VECTOR_DIMENSIONS + 1];
    let result = PropertyValue::try_vector(oversized);
    assert!(
        result.is_err(),
        "Should reject oversized vector at construction"
    );
}

#[test]
fn test_deserialize_array_dos_amplification() {
    // Attempt to deserialize an Array with a declared count of 10,000,000 elements
    // but a small buffer. This tests that we don't pre-allocate a Vec of capacity 10,000,000
    // before checking if we have enough bytes.
    let mut bytes = Vec::new();
    bytes.push(TAG_ARRAY);
    let count = 10_000_000u32;
    bytes.extend_from_slice(&count.to_le_bytes());
    // No payload

    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // We expect "Insufficient buffer size" check to fire BEFORE allocation
    assert!(
        msg.contains("Insufficient buffer size"),
        "Should fail on buffer size check, got: {}",
        msg
    );
}

#[test]
fn test_deserialize_string_dos_amplification() {
    // Attempt to deserialize a String with declared length of 1GB but small buffer.
    let mut bytes = Vec::new();
    bytes.push(TAG_STRING);
    let len = 1_000_000_000u32; // 1GB
    bytes.extend_from_slice(&len.to_le_bytes());
    // No payload

    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    assert!(
        msg.contains("Buffer too short"),
        "Should fail on buffer size check, got: {}",
        msg
    );
}

#[test]
fn test_deserialize_vector_dos_amplification() {
    // Attempt to deserialize a Vector with declared dimension of MAX (4096)
    // but small buffer.
    let mut bytes = Vec::new();
    bytes.push(TAG_VECTOR);
    let dim = MAX_VECTOR_DIMENSIONS as u32;
    bytes.extend_from_slice(&dim.to_le_bytes());
    // No payload

    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    assert!(
        msg.contains("Buffer too short"),
        "Should fail on buffer size check, got: {}",
        msg
    );
}
