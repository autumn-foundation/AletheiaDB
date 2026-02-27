#[cfg(test)]
mod sentry_vector_serialization {
    use aletheiadb::core::vector::serialization::{deserialize_vector, serialize_vector};

    #[test]
    fn test_deserialize_unaligned_slice() {
        // Create a vector
        let v: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let bytes = serialize_vector(&v);

        // Create a buffer with offset to force unaligned access
        let mut buffer = vec![0u8; bytes.len() + 1];
        // Skip first byte
        buffer[1..].copy_from_slice(&bytes);

        // Attempt deserialize from unaligned slice
        // The slice `&buffer[1..]` starts at an odd address
        let (deserialized, consumed) =
            deserialize_vector(&buffer[1..]).expect("Should handle unaligned slice");

        assert_eq!(deserialized.len(), v.len());
        assert_eq!(consumed, bytes.len());

        // Verify values
        for (i, val) in deserialized.iter().enumerate() {
            assert_eq!(*val, v[i]);
        }
    }
}
