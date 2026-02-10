use proptest::prelude::*;
use super::property::*;

proptest! {
    #[test]
    fn test_vector_roundtrip(
        v in proptest::collection::vec(proptest::num::f32::ANY, 0..2048)
    ) {
        let serialized = serialize_vector(&v);
        let (deserialized, consumed) = deserialize_vector(&serialized).unwrap();

        assert_eq!(consumed, serialized.len());
        assert_eq!(deserialized.len(), v.len());

        for (i, (a, b)) in v.iter().zip(deserialized.iter()).enumerate() {
            if a.is_nan() {
                assert!(b.is_nan(), "Index {}: expected NaN, got {}", i, b);
            } else {
                // Exact bitwise equality is expected because:
                // 1. On little-endian, we use bulk copy (unsafe)
                // 2. On big-endian, we use to_le_bytes/from_le_bytes which preserves bits
                assert_eq!(a.to_bits(), b.to_bits(), "Index {}: bit mismatch", i);
            }
        }
    }

    #[test]
    fn test_sparse_vector_roundtrip(
        dim in 1u32..10_000,
        indices in proptest::collection::vec(proptest::num::u32::ANY, 0..1000),
        values in proptest::collection::vec(proptest::num::f32::ANY, 0..1000),
    ) {
        // Construct a valid sparse vector manually to handle constraints
        let len = std::cmp::min(indices.len(), values.len());
        // Ensure indices are < dimension
        let mut valid_indices: Vec<u32> = indices.iter().take(len).map(|&i| i % dim).collect();
        valid_indices.sort();
        valid_indices.dedup();

        let valid_values: Vec<f32> = values.iter().take(valid_indices.len())
            .map(|&v| if v == 0.0 { 1.0 } else { v }) // No zeros allowed
            .collect();

        // Might have lost elements due to dedup, trim values
        let count = std::cmp::min(valid_indices.len(), valid_values.len());
        let final_indices = valid_indices[0..count].to_vec();
        let final_values = valid_values[0..count].to_vec();

        // Skip if we ended up with invalid NaNs or Infs if SparseVec doesn't support them
        // SparseVec::new checks for NaN/Inf.
        if final_values.iter().any(|v| v.is_nan() || v.is_infinite()) {
            return Ok(());
        }

        if let Ok(sv) = crate::core::vector::SparseVec::new(final_indices, final_values, dim) {
            let serialized = serialize_sparse_vector(&sv);
            let (deserialized_arc, consumed) = deserialize_sparse_vector(&serialized).unwrap();

            assert_eq!(consumed, serialized.len());
            assert_eq!(deserialized_arc.dimension(), sv.dimension());
            assert_eq!(deserialized_arc.nnz(), sv.nnz());
            assert_eq!(deserialized_arc.indices(), sv.indices());

            // Check values
            for (a, b) in sv.values().iter().zip(deserialized_arc.values().iter()) {
                 assert_eq!(a.to_bits(), b.to_bits());
            }
        }
    }
}
