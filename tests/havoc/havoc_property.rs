use aletheiadb::core::PropertyValue;
use aletheiadb::core::property::MAX_VECTOR_DIMENSIONS;
use aletheiadb::core::vector::SparseVec;
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
                msg.contains("Empty buffer") ||
                msg.contains("Insufficient buffer size"),
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

    #[test]
    fn prop_round_trip(val in arb_property_value()) {
        let serialized = val.serialize().expect("Serialization should succeed");
        let (deserialized, consumed) = PropertyValue::deserialize(&serialized).expect("Deserialization should succeed");

        assert_eq!(consumed, serialized.len());

        // Handle NaN float comparison
        match &val {
            PropertyValue::Float(f) if f.is_nan() => {
                assert!(deserialized.as_float().unwrap().is_nan());
                return Ok(());
            }
            _ => {}
        }
        // Handle NaN inside vector/array?
        // For now, let's assume PartialEq handles it or we accept failure if random f32 is NaN.
        // Actually PropertyValue::PartialEq for float checks exact bits or standard equality.
        // If we generate NaN, normal == might fail.
        // Our strategy generates any::<f64>() which includes NaN.

        // But wait, PropertyValue derived PartialEq.
        // f64::NAN != f64::NAN.
        // So we should filter out NaN from our strategy or handle it.
        // Let's rely on string format comparison for robust round-trip of NaN?
        // Or just assert equality if not NaN.

        // Let's filter NaN from strategy for simplicity of equality check

        // Actually, let's verify via debug string if direct equality fails,
        // or just accept that we generate non-NaN floats.

        if val != deserialized {
             // Fallback check for NaN
             let s1 = format!("{:?}", val);
             let s2 = format!("{:?}", deserialized);
             assert_eq!(s1, s2, "Values not equal (even via Debug): {:?} vs {:?}", val, deserialized);
        }
    }
}

fn arb_sparse_vector() -> impl Strategy<Value = PropertyValue> {
    (1..1000u32).prop_flat_map(|dim| {
        let max_nnz = (dim as usize).min(50);
        // Generate random (index, value) pairs.
        // Value must not be 0.0 (sparse invariant) or NaN (for equality check stability).
        let val_strat =
            any::<f32>().prop_filter("Valid sparse value", |&v| v != 0.0 && !v.is_nan());
        let idx_strat = 0..dim;

        prop::collection::vec((idx_strat, val_strat), 0..max_nnz)
            .prop_map(move |pairs| {
                // Deduplicate indices (using BTreeMap sorts by index automatically)
                let mut map = std::collections::BTreeMap::new();
                for (idx, val) in pairs {
                    map.insert(idx, val);
                }

                let (indices, values): (Vec<u32>, Vec<f32>) = map.into_iter().unzip();

                // Construct SparseVec
                SparseVec::new(indices, values, dim).expect("Invalid sparse vector generated")
            })
            .prop_map(PropertyValue::sparse_vector)
    })
}

fn arb_property_value() -> impl Strategy<Value = PropertyValue> {
    let leaf = prop_oneof![
        Just(PropertyValue::Null),
        any::<bool>().prop_map(PropertyValue::Bool),
        any::<i64>().prop_map(PropertyValue::Int),
        // Filter out NaN to make equality checks easier
        any::<f64>()
            .prop_filter("No NaN", |f| !f.is_nan())
            .prop_map(PropertyValue::Float),
        ".*".prop_map(PropertyValue::string),
        prop::collection::vec(any::<u8>(), 0..100).prop_map(PropertyValue::bytes),
        prop::collection::vec(any::<f32>().prop_filter("No NaN", |f| !f.is_nan()), 0..100)
            .prop_map(PropertyValue::vector),
        arb_sparse_vector(),
    ];

    leaf.prop_recursive(3, 64, 5, |inner| {
        prop::collection::vec(inner, 0..5).prop_map(PropertyValue::array)
    })
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
    if let PropertyValue::Vector(ref v) = val {
        assert_eq!(v.len(), 4);
    } else {
        panic!("Expected Vector");
    }
}
