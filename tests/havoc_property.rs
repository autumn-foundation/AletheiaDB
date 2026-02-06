use aletheiadb::core::PropertyValue;
use aletheiadb::core::property::MAX_VECTOR_DIMENSIONS;
use proptest::prelude::*;

const TAG_VECTOR: u8 = 7;
const TAG_SPARSE_VECTOR: u8 = 8;
const TAG_ARRAY: u8 = 6;
const TAG_NULL: u8 = 0;

proptest! {
    // Run enough cases to have a chance of hitting edge cases, but not too many for CI
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn fuzz_deserialize_arbitrary(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        // The goal is to ensure this never panics, regardless of input
        let result = PropertyValue::deserialize(&bytes);
        if let Err(e) = result {
            let msg = e.to_string();
            // Verify error messages are reasonable
            assert!(
                msg.contains("Buffer too short") ||
                msg.contains("Invalid") ||
                msg.contains("overflow") ||
                msg.contains("exceeded") ||
                msg.contains("exceeds maximum allowed") ||
                msg.contains("exceeds dimension") ||
                msg.contains("Unknown PropertyValue type tag") ||
                msg.contains("Empty buffer"),
                "Unexpected error message: {}", msg
            );
        }
    }

    #[test]
    fn fuzz_deserialize_vector_structure(
        dim in any::<u32>(),
        data in prop::collection::vec(any::<u8>(), 0..1024)
    ) {
        // Construct a buffer that looks like a vector but has random dimension/data
        let mut bytes = vec![TAG_VECTOR];
        bytes.extend_from_slice(&dim.to_le_bytes());
        bytes.extend_from_slice(&data);

        // This exercises the unsafe block in deserialize_vector
        let result = PropertyValue::deserialize(&bytes);

        if (dim as usize) > MAX_VECTOR_DIMENSIONS {
             if let Err(e) = result {
                assert!(e.to_string().contains("exceeds maximum allowed"), "Should fail with size error");
             } else {
                 panic!("Should have failed due to dimension > MAX_VECTOR_DIMENSIONS");
             }
        }
    }

    #[test]
    fn fuzz_deserialize_sparse_vector_structure(
        dim in any::<u32>(),
        nnz in any::<u32>(),
        data in prop::collection::vec(any::<u8>(), 0..1024)
    ) {
        // Construct a buffer that looks like a sparse vector
        let mut bytes = vec![TAG_SPARSE_VECTOR];
        bytes.extend_from_slice(&dim.to_le_bytes());
        bytes.extend_from_slice(&nnz.to_le_bytes());
        bytes.extend_from_slice(&data);

        let _ = PropertyValue::deserialize(&bytes);
    }

    #[test]
    fn fuzz_recursion_depth(depth in 0..200usize) {
        // Construct nested arrays to test recursion limit
        let mut bytes = Vec::new();
        for _ in 0..depth {
            bytes.push(TAG_ARRAY);
            bytes.extend_from_slice(&1u32.to_le_bytes()); // Count 1
        }
        bytes.push(TAG_NULL); // Terminate with Null

        let _ = PropertyValue::deserialize(&bytes);
    }
}

#[test]
fn test_exact_max_vector_dimensions() {
    let mut bytes = vec![TAG_VECTOR];
    let dim = MAX_VECTOR_DIMENSIONS as u32;
    bytes.extend_from_slice(&dim.to_le_bytes());
    // Create valid data for this dimension
    // Need dim * 4 bytes
    let data = vec![0u8; MAX_VECTOR_DIMENSIONS * 4];
    bytes.extend_from_slice(&data);

    let result = PropertyValue::deserialize(&bytes);
    assert!(
        result.is_ok(),
        "Should succeed at exact MAX_VECTOR_DIMENSIONS"
    );
}

#[test]
fn test_integer_overflow_dimension() {
    let mut bytes = vec![TAG_VECTOR];
    // u32::MAX causes (dim * 4) to wrap around if checked_mul isn't used
    let dim = u32::MAX;
    bytes.extend_from_slice(&dim.to_le_bytes());
    // Provide some data, but not enough for u32::MAX
    bytes.extend_from_slice(&[0u8; 100]);

    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("exceeds maximum allowed"),
        "Should be caught by MAX check first: {}",
        err
    );
}

#[test]
fn test_unaligned_vector_access() {
    // Construct a valid vector buffer
    let mut vec_bytes = vec![TAG_VECTOR];
    let dim = 4u32;
    vec_bytes.extend_from_slice(&dim.to_le_bytes());
    let data = vec![0u8; 16]; // 4 * 4 bytes
    vec_bytes.extend_from_slice(&data);

    // Embed it in a larger buffer at an unaligned offset (e.g. index 1)
    let mut buffer = vec![0xFF]; // 1 byte padding
    buffer.extend_from_slice(&vec_bytes);

    // Parse from offset 1
    let result = PropertyValue::deserialize(&buffer[1..]);
    assert!(
        result.is_ok(),
        "Should handle unaligned access via copy_nonoverlapping"
    );
    let (val, consumed) = result.unwrap();
    assert_eq!(consumed, vec_bytes.len());
    if let PropertyValue::Vector(v) = val {
        assert_eq!(v.len(), 4);
    } else {
        panic!("Expected Vector");
    }
}
