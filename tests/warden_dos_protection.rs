use gallifreydb::core::property::{PropertyMap, TAG_SPARSE_VECTOR, deserialize_sparse_vector};

#[test]
fn test_property_map_allocation_limit() {
    // Attempt to deserialize a PropertyMap with count = 1,000,001
    // This should trigger the new security check (limit 10,000)
    let count = 1_000_001u32;
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&count.to_le_bytes());

    // We don't provide the actual data.
    // Without the fix, this would allocate HashMap(1_000_001) and then fail with
    // "Buffer too short". With the fix, this should fail immediately with
    // "exceeds maximum allowed".

    let result = PropertyMap::deserialize(&buffer);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("exceeds maximum allowed"),
        "Expected error about maximum capacity, got: {}",
        err
    );
}

#[test]
fn test_sparse_vector_allocation_limit() {
    // Attempt to deserialize a SparseVector with nnz = 100,001
    // This should trigger the check against MAX_VECTOR_DIMENSIONS (100,000)
    let dimension = 200_000u32; // Valid dimension
    let nnz = 100_001u32; // Invalid nnz (over limit)

    let mut buffer = Vec::new();
    buffer.push(TAG_SPARSE_VECTOR);
    buffer.extend_from_slice(&dimension.to_le_bytes());
    buffer.extend_from_slice(&nnz.to_le_bytes());

    // Without fix: allocates Vec(100_001) -> fail "Buffer too short"
    // With fix: fail "exceeds maximum allowed"

    let result = deserialize_sparse_vector(&buffer);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("exceeds maximum allowed"),
        "Expected error about maximum capacity, got: {}",
        err
    );
}
