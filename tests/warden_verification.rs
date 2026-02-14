use aletheiadb::core::property::{
    MAX_RECURSION_DEPTH, MAX_VECTOR_DIMENSIONS, PropertyValue, TAG_SPARSE_VECTOR, TAG_VECTOR,
    deserialize_sparse_vector, deserialize_vector,
};
use aletheiadb::utils::error::{Error, StorageError, VectorError};

// 🔒 Warden Security Verification Tests
// These tests act as "Test Exploits" to verify that security controls are active and effective.

#[test]
fn test_warden_deserialize_vector_dos_prevention() {
    // 🦠 Threat: Attacker provides a vector dimension claiming 2 billion elements.
    // Without checks, this would cause massive allocation (8GB) and crash the server (DoS).

    let mut buffer = Vec::new();
    buffer.push(TAG_VECTOR);

    // Dimension: MAX_VECTOR_DIMENSIONS + 1 (Little Endian)
    let invalid_dim = (MAX_VECTOR_DIMENSIONS + 1) as u32;
    buffer.extend_from_slice(&invalid_dim.to_le_bytes());

    // We don't even need to provide payload data, validation should happen first.

    let result = deserialize_vector(&buffer);

    // 🛡️ Defense Verification
    assert!(result.is_err());
    let err = result.unwrap_err();

    match err {
        Error::Vector(VectorError::DimensionTooLarge {
            dimension,
            max_allowed,
        }) => {
            assert_eq!(dimension, (MAX_VECTOR_DIMENSIONS + 1));
            assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
        }
        _ => panic!("Expected DimensionTooLarge error, got {:?}", err),
    }
}

#[test]
fn test_warden_deserialize_vector_buffer_overflow_prevention() {
    // 🦠 Threat: Attacker provides a valid large dimension but truncated data.
    // Unsafe code might read past end of buffer if length check is missing.

    let mut buffer = Vec::new();
    buffer.push(TAG_VECTOR);

    let dim = 100u32;
    buffer.extend_from_slice(&dim.to_le_bytes());

    // Provide some data, but not enough (need 400 bytes, provide 10)
    buffer.extend_from_slice(&[0u8; 10]);

    let result = deserialize_vector(&buffer);

    // 🛡️ Defense Verification
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Storage(StorageError::CorruptedData(msg)) => {
            assert!(msg.contains("Buffer too short"));
        }
        err => panic!("Expected CorruptedData error, got {:?}", err),
    }
}

#[test]
fn test_warden_recursion_depth_limit() {
    // 🦠 Threat: Stack overflow via deeply nested structures (JSON bomb style).

    // Construct a PropertyValue that exceeds MAX_RECURSION_DEPTH
    let depth = MAX_RECURSION_DEPTH + 5;
    let mut val = PropertyValue::Int(42);

    for _ in 0..depth {
        val = PropertyValue::array(vec![val]);
    }

    // Serialization should fail
    let result = val.serialize();

    // 🛡️ Defense Verification
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Storage(StorageError::CorruptedData(msg)) => {
            // Note: serialize() fails early in serialized_size() with a specific message
            assert!(
                msg.to_lowercase()
                    .contains("recursion depth limit exceeded"),
                "Error message '{}' should contain 'recursion depth limit exceeded'",
                msg
            );
        }
        err => panic!("Expected recursion limit error, got {:?}", err),
    }
}

#[test]
fn test_warden_sparse_vector_validation_duplicates() {
    // 🦠 Threat: Sparse vector with duplicate indices violates invariant.

    let mut buffer = Vec::new();
    buffer.push(TAG_SPARSE_VECTOR);

    let dim = 100u32;
    let nnz = 2u32;
    buffer.extend_from_slice(&dim.to_le_bytes());
    buffer.extend_from_slice(&nnz.to_le_bytes());

    // Indices: [10, 10] (Duplicate!)
    buffer.extend_from_slice(&10u32.to_le_bytes());
    buffer.extend_from_slice(&10u32.to_le_bytes());

    // Values: [1.0, 2.0]
    buffer.extend_from_slice(&1.0f32.to_le_bytes());
    buffer.extend_from_slice(&2.0f32.to_le_bytes());

    let result = deserialize_sparse_vector(&buffer);

    // 🛡️ Defense Verification
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Vector(VectorError::InvalidSparseVector { reason }) => {
            assert!(reason.contains("Duplicate index"));
        }
        err => panic!("Expected InvalidSparseVector error, got {:?}", err),
    }
}

#[test]
fn test_warden_sparse_vector_validation_zeros() {
    // 🦠 Threat: Sparse vector storing zero values (inefficient and potentially logic breaking).

    let mut buffer = Vec::new();
    buffer.push(TAG_SPARSE_VECTOR);

    let dim = 100u32;
    let nnz = 1u32;
    buffer.extend_from_slice(&dim.to_le_bytes());
    buffer.extend_from_slice(&nnz.to_le_bytes());

    // Index: 5
    buffer.extend_from_slice(&5u32.to_le_bytes());

    // Value: 0.0 (Invalid)
    buffer.extend_from_slice(&0.0f32.to_le_bytes());

    let result = deserialize_sparse_vector(&buffer);

    // 🛡️ Defense Verification
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Vector(VectorError::InvalidSparseVector { reason }) => {
            assert!(reason.contains("zero value"));
        }
        err => panic!("Expected InvalidSparseVector error, got {:?}", err),
    }
}
