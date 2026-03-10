#[cfg(test)]
mod tests {
    use crate::core::vector::serialization::{deserialize_vector, deserialize_sparse_vector};
    use crate::core::error::Error;

    #[test]
    fn test_deserialize_vector_short_buffer() {
        // Vector with TAG_VECTOR (0x07) and valid dimension (2) but missing data
        // TAG(1) + DIM(4) = 5 bytes total. We need 13 bytes total (5 + 2*4)
        let bytes = [crate::core::property::TAG_VECTOR, 2, 0, 0, 0, 0, 0, 0]; // 8 bytes long

        let result = deserialize_vector(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for vector data:"));
        } else {
            panic!("Expected CorruptedData error for short vector buffer");
        }
    }

    #[test]
    fn test_deserialize_vector_missing_dimension() {
        // Only TAG_VECTOR
        let bytes = [crate::core::property::TAG_VECTOR, 1, 0];
        let result = deserialize_vector(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for vector header"));
        } else {
            panic!("Expected CorruptedData error for missing dimension");
        }
    }

    #[test]
    fn test_deserialize_sparse_vector_missing_metadata() {
        // Missing nnz or dimension
        let bytes = [crate::core::property::TAG_SPARSE_VECTOR, 5, 0, 0, 0, 1, 0]; // only 7 bytes, need 9 for meta
        let result = deserialize_sparse_vector(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for sparse vector header"));
        } else {
            panic!("Expected CorruptedData error for missing metadata");
        }
    }

    #[test]
    fn test_deserialize_sparse_vector_short_indices_buffer() {
        // Dimension = 10, nnz = 2. Needs 9 + 2*4 + 2*4 = 25 bytes.
        // Provide only 15 bytes (short on indices)
        let mut bytes = vec![crate::core::property::TAG_SPARSE_VECTOR];
        bytes.extend_from_slice(&10u32.to_le_bytes()); // dim
        bytes.extend_from_slice(&2u32.to_le_bytes());  // nnz
        bytes.extend_from_slice(&[0, 0, 0, 0]); // one index, missing second

        let result = deserialize_sparse_vector(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for sparse vector data:"));
        } else {
            panic!("Expected CorruptedData error for short indices buffer");
        }
    }

    #[test]
    fn test_deserialize_sparse_vector_short_values_buffer() {
        // Dimension = 10, nnz = 2. Needs 9 + 2*4 + 2*4 = 25 bytes.
        // Provide enough for indices, but short on values
        let mut bytes = vec![crate::core::property::TAG_SPARSE_VECTOR];
        bytes.extend_from_slice(&10u32.to_le_bytes()); // dim
        bytes.extend_from_slice(&2u32.to_le_bytes());  // nnz
        bytes.extend_from_slice(&0u32.to_le_bytes());  // index 1
        bytes.extend_from_slice(&1u32.to_le_bytes());  // index 2
        bytes.extend_from_slice(&1.0f32.to_le_bytes());// value 1, missing value 2

        let result = deserialize_sparse_vector(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for sparse vector data:"));
        } else {
            panic!("Expected CorruptedData error for short values buffer");
        }
    }
}
