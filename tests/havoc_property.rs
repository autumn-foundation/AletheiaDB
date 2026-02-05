use gallifreydb::core::PropertyValue;
use proptest::prelude::*;

const TAG_VECTOR: u8 = 7;
const TAG_SPARSE_VECTOR: u8 = 8;
const TAG_ARRAY: u8 = 6;
const TAG_NULL: u8 = 0;

proptest! {
    // Run enough cases to have a chance of hitting edge cases
    #![proptest_config(ProptestConfig::with_cases(5000))]

    #[test]
    fn fuzz_deserialize_arbitrary(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        // The goal is to ensure this never panics, regardless of input
        let _ = PropertyValue::deserialize(&bytes);
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
        let _ = PropertyValue::deserialize(&bytes);
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
