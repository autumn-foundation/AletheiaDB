//! Property-based fuzzing and regression tests for Property deserialization.
//! Sentry's shield against corruption and DoS attacks. 🛡️

use aletheiadb::core::property::{
    MAX_ARRAY_ELEMENTS, MAX_PROPERTY_MAP_CAPACITY, MAX_VECTOR_DIMENSIONS, PropertyMap,
    PropertyValue, deserialize_sparse_vector,
};
use aletheiadb::utils::error::{Error, StorageError, VectorError};
use proptest::prelude::*;

const TAG_VECTOR: u8 = 7;
const TAG_SPARSE_VECTOR: u8 = 8;
const TAG_ARRAY: u8 = 6;

proptest! {
    // Fuzz PropertyMap deserialization with completely arbitrary bytes
    #[test]
    fn test_deserialize_property_map_arbitrary(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let result = PropertyMap::deserialize(&bytes);
        if let Err(e) = result {
            let msg = e.to_string();
            // Verify error messages are reasonable and not panics
            // We expect corrupted data, insufficient buffer, or UTF-8 errors
            assert!(
                msg.contains("Corrupted data") ||
                msg.contains("Insufficient buffer") ||
                msg.contains("Buffer too short") ||
                msg.contains("Invalid UTF-8") ||
                msg.contains("exceeds maximum allowed") ||
                msg.contains("recursion depth limit") ||
                msg.contains("Duplicate property key") ||
                msg.contains("mismatch"),
                "Unexpected error message: {}", msg
            );
        }
    }

    // Fuzz SparseVec deserialization structure
    #[test]
    fn test_deserialize_sparse_vector_structure_fuzz(
        dim in any::<u32>(),
        nnz in any::<u32>(),
        indices in prop::collection::vec(any::<u32>(), 0..100), // indices bytes
        values in prop::collection::vec(any::<f32>(), 0..100)   // values bytes
    ) {
        // Construct a buffer that mimics sparse vector structure
        // [tag][dim][nnz][indices][values]
        let mut bytes = vec![TAG_SPARSE_VECTOR];
        bytes.extend_from_slice(&dim.to_le_bytes());
        bytes.extend_from_slice(&nnz.to_le_bytes());

        // We use raw bytes for indices/values to simulate corruption
        // Convert u32s to bytes
        for idx in indices {
            bytes.extend_from_slice(&idx.to_le_bytes());
        }
        for val in values {
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        let result = deserialize_sparse_vector(&bytes);

        if let Err(e) = result {
            let msg = e.to_string();
            // Expected errors
            assert!(
                msg.contains("Buffer too short") ||
                msg.contains("Corrupted data") ||
                msg.contains("dimension") ||
                msg.contains("Invalid sparse vector") ||
                msg.contains("overflow") ||
                msg.contains("exceeds maximum allowed"),
                "Unexpected error message: {}", msg
            );
        } else {
            // If it succeeds, the sparse vector must be valid
            let (sv, consumed) = result.unwrap();
            assert!(consumed <= bytes.len());
            assert_eq!(sv.dimension(), dim as usize);
            assert_eq!(sv.nnz(), nnz as usize);
            // Invariants checked by constructor: sorted indices, unique, no zeros
        }
    }
}

#[test]
fn test_deserialize_property_map_with_array_limit() {
    // 💣 Risk: Malicious input claiming huge array size causes DoS or panic.
    // 🧪 Strategy: Construct a serialized Array property with count > MAX_ARRAY_ELEMENTS.

    let mut bytes = vec![TAG_ARRAY];
    let count = (MAX_ARRAY_ELEMENTS + 1) as u32;
    bytes.extend_from_slice(&count.to_le_bytes());

    // We don't need to provide payload, the count check should fail first

    let result = PropertyValue::deserialize(&bytes);

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Storage(StorageError::CorruptedData(msg)) => {
            assert!(msg.contains("exceeds maximum allowed"));
            assert!(msg.contains(&count.to_string()));
        }
        e => panic!("Expected StorageError::CorruptedData, got {:?}", e),
    }
}

#[test]
fn test_deserialize_property_map_capacity_limit() {
    // 💣 Risk: Malicious input claiming huge map size causes OOM DoS.
    // 🧪 Strategy: Construct a serialized PropertyMap with count > MAX_PROPERTY_MAP_CAPACITY.

    let mut bytes = Vec::new();
    let count = (MAX_PROPERTY_MAP_CAPACITY + 1) as u32;
    bytes.extend_from_slice(&count.to_le_bytes());

    let result = PropertyMap::deserialize(&bytes);

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Storage(StorageError::CorruptedData(msg)) => {
            assert!(msg.contains("exceeds maximum allowed"));
        }
        e => panic!("Expected StorageError::CorruptedData, got {:?}", e),
    }
}

#[test]
fn test_deserialize_with_duplicate_key() {
    // 💣 Risk: Database corruption or ambiguous state if duplicate keys are allowed.
    // 🧪 Strategy: Manually construct a serialized PropertyMap with duplicate keys.
    //    PropertyMap::deserialize must reject this with CorruptedData error.

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2u32.to_le_bytes()); // Count 2

    // Key "a"
    let key = "a";
    bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
    bytes.extend_from_slice(key.as_bytes());
    PropertyValue::Int(1).serialize_into(&mut bytes).unwrap();

    // Key "a" (duplicate)
    bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
    bytes.extend_from_slice(key.as_bytes());
    PropertyValue::Int(2).serialize_into(&mut bytes).unwrap();

    let result = PropertyMap::deserialize(&bytes);
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Storage(StorageError::CorruptedData(msg)) => {
            assert!(msg.contains("Duplicate property key"));
        }
        e => panic!(
            "Expected StorageError::CorruptedData for duplicate key, got {:?}",
            e
        ),
    }
}

#[test]
fn test_deserialize_vector_exceeding_max_dimensions() {
    // 💣 Risk: Allocation DoS via huge vector dimension.
    // 🧪 Strategy: Construct Vector header with dimension > MAX_VECTOR_DIMENSIONS.

    let mut bytes = vec![TAG_VECTOR];
    let dim = (MAX_VECTOR_DIMENSIONS + 1) as u32;
    bytes.extend_from_slice(&dim.to_le_bytes());

    let result = PropertyValue::deserialize(&bytes);

    assert!(result.is_err());
    // Should fail validation before even checking buffer length
    match result.unwrap_err() {
        Error::Vector(VectorError::DimensionTooLarge {
            dimension,
            max_allowed,
        }) => {
            assert_eq!(dimension, dim as usize);
            assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
        }
        e => panic!("Expected VectorError::DimensionTooLarge, got {:?}", e),
    }
}
