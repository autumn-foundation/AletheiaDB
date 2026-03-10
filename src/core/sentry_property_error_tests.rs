#[cfg(test)]
mod tests {
    use crate::core::property::{PropertyValue, PropertyMap};
    use crate::core::error::Error;
    use crate::core::property::{TAG_INT, TAG_FLOAT, TAG_STRING, TAG_BYTES, TAG_ARRAY};

    #[test]
    fn test_deserialize_int_short_buffer() {
        // Int requires 1 byte for tag and 8 bytes for value.
        let bytes = [TAG_INT, 1, 2, 3]; // Only 4 bytes total
        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for Int value"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }

    #[test]
    fn test_deserialize_float_short_buffer() {
        // Float requires 1 byte for tag and 8 bytes for value.
        let bytes = [TAG_FLOAT, 1, 2, 3, 4, 5]; // Only 6 bytes total
        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for Float value"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }

    #[test]
    fn test_deserialize_string_short_length_buffer() {
        // String requires 1 byte for tag, 4 for length
        let bytes = [TAG_STRING, 1, 0]; // 3 bytes total, length is incomplete
        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for String length"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }

    #[test]
    fn test_deserialize_string_short_data_buffer() {
        // String claims 10 bytes length, but data is missing
        let mut bytes = vec![TAG_STRING];
        bytes.extend_from_slice(&10u32.to_le_bytes()); // len = 10
        bytes.extend_from_slice(&[65, 66, 67]); // Only 3 bytes of data

        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for String data"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }

    #[test]
    fn test_deserialize_bytes_short_length_buffer() {
        let bytes = [TAG_BYTES, 1, 0];
        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for Bytes length"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }

    #[test]
    fn test_deserialize_bytes_short_data_buffer() {
        let mut bytes = vec![TAG_BYTES];
        bytes.extend_from_slice(&10u32.to_le_bytes()); // len = 10
        bytes.extend_from_slice(&[1, 2, 3]);

        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for Bytes data"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }

    #[test]
    fn test_deserialize_array_short_count_buffer() {
        let bytes = [TAG_ARRAY, 1, 0];
        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for Array count"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }

    #[test]
    fn test_deserialize_property_map_short_count_buffer() {
        let bytes = [1, 0, 0]; // 3 bytes, short for 4 byte count
        let result = PropertyMap::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            assert!(msg.contains("Buffer too short for PropertyMap count"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }

    #[test]
    fn test_deserialize_property_map_short_key_length_buffer() {
        let mut bytes = vec![];
        bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 entry
        bytes.extend_from_slice(&[1, 0]); // only 2 bytes for key length
        let result = PropertyMap::deserialize(&bytes);
        assert!(result.is_err());
        if let Err(Error::Storage(crate::core::error::StorageError::CorruptedData(msg))) = result {
            // It could hit the min_required_bytes check first, or the key length check
            assert!(msg.contains("Insufficient buffer size for PropertyMap entries") || msg.contains("Buffer too short for property key length"));
        } else {
            panic!("Expected CorruptedData error");
        }
    }
}
